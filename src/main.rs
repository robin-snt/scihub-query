#[macro_use]
extern crate clap;
extern crate serde;
extern crate serde_derive;
extern crate serde_xml_rs;
extern crate reqwest;
extern crate confy;
extern crate tokio;
extern crate wkt;
extern crate geo;

use clap::{Arg, App};

use std::io::{self, Read};
use std::io::prelude::*;
use std::fs::File;
use std::path::Path;
use std::convert::Into;
use std::{fmt, error::Error, env::var};
use read_input::prelude::*;

use serde::{Serialize, Deserialize};
use futures::{stream, StreamExt};
use reqwest::Client;
use wkt::ToWkt;
use wkt::conversion::try_into_geometry;
use geo::Geometry;
use geo::algorithm::simplify::{Simplify};

// 13 represents &start=XXXXXXX
static REQUEST_LIMIT:usize = 2048 - 13;

#[derive(Deserialize, Debug)]
struct Feed {
    #[serde(rename = "totalResults")]
    pub total_results: u64,
    pub entry: Option<Vec<Entry>>,
}

#[derive(Deserialize, Debug)]
struct Entry {
    title: String,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct ScihubConfig {
    username: String,
    password: String,
}

// Specific error types and traits to convert error from output type of Url::set_* to output type
// of reqwest::blocking::get() ..

#[derive(Debug)]
enum ScihubCredentialError {
    Credentials(())
}

trait ScihubBasicAuth {
    fn set_scihub_auth(&mut self, cfg: &ScihubConfig) -> Result<(), ScihubCredentialError>;
}

impl From<()> for ScihubCredentialError {
    fn from(_: ()) -> ScihubCredentialError {
        ScihubCredentialError::Credentials(())
    }
}

impl ScihubBasicAuth for reqwest::Url {
    fn set_scihub_auth(&mut self, cfg: &ScihubConfig) -> Result<(), ScihubCredentialError> {
        self.set_username(&cfg.username.as_str())?;
        self.set_password(Some(&cfg.password.as_str()))?;
        Ok(())
    }
}

impl fmt::Display for ScihubCredentialError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Error when setting credentials!")
    }
}

impl Error for ScihubCredentialError {}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let m = App::new(crate_name!())
        .version(crate_version!())
        .author(crate_authors!())
        .about("Query scihub")
        .arg(Arg::with_name("BEGIN")
            .short("b")
            .long("begin-date")
            .takes_value(true)
            .required_unless("STORECREDS")
            .help("Date range start (YYYY-MM-DD)"))
        .arg(Arg::with_name("END")
            .short("e")
            .long("end-date")
            .takes_value(true)
            .default_value("NOW")
            .help("Date range end (YYYY-MM-DD)"))
        .arg(Arg::with_name("PRODUCT")
            .short("p")
            .long("product-type")
            .takes_value(true)
            .help("S2 product type")
            .possible_values(&["1C", "2A"])
            .default_value("1C"))
        .arg(Arg::with_name("WKT")
            .takes_value(true)
            .required_unless("STORECREDS")
            .help("Specify ROI wkt file (`-` for stdin)"))
        .arg(Arg::with_name("CCOVER")
            .short("c")
            .long("cloud-cover")
            .takes_value(true)
            .help("Cloud cover percentage. 0 (clear sky) - 100 (complete cover)"))
        .arg(Arg::with_name("RELATIVEORBIT")
            .short("r")
            .long("relative-orbit")
            .takes_value(true)
            .multiple(true)
            .number_of_values(1)
            .help("Relative orbit integer: 1-143"))
        .arg(Arg::with_name("STORECREDS")
            .short("s")
            .long("store-credentials")
            .help("Write new scihub credentials"))
        .arg(Arg::with_name("LIMIT")
            .short("l")
            .long("limit")
            .takes_value(true)
            .help("Entries capped at some LIMIT greater than 100."))
        .arg(Arg::with_name("QUERYSTRING")
            .long("query-string")
            .help("Print scihub query string and exit"))
        .arg(Arg::with_name("DUMPWKT")
            .long("dump-wkt")
            .takes_value(true)
            .default_value("simplified.wkt")
            .help("Write simplified WKT to file"))
        .arg(Arg::with_name("TILE")
            .short("t")
            .long("tile-filter")
            .takes_value(true)
            .help("Only return products with specified S2 tile id"))
    .get_matches();

    if m.is_present("STORECREDS") {
        manage_config();
        return Ok(())
    }

    let cfg: ScihubConfig = read_creds_from_env()
        .unwrap_or(confy::load(crate_name!())?);

    let mut wkt_str = String::new();

    match m.value_of("WKT").unwrap() {
        "-" => {
            let stdin = io::stdin();
            let mut handle = stdin.lock();
            handle.read_to_string(&mut wkt_str).expect("Error reading from stdin!");
        },
        filename => {
            let mut f = File::open(filename).expect("file not found");
            f.read_to_string(&mut wkt_str).expect("Error reading from file!");
        }
    }

    // TODO: support human time format
    let begin_time = valid_date(m.value_of("BEGIN").unwrap());
    // let begin_time = format!("{}T00:00:00.000Z", m.value_of("BEGIN").unwrap());

    let end_time = match m.value_of("END").unwrap() {
        "NOW" => "NOW".to_string(),
        dt => valid_date(dt)
    };

    let product_type = m.value_of("PRODUCT").unwrap();

    let ccover = m.value_of("CCOVER")
        .map(|s| {
            match s.parse::<usize>() {
                Err(_) => panic!("Cloud cover must be integer between 0 and 100!"),
                Ok(mut i) => {
                    if i > 100 {
                        i = 100;
                    }

                    format!("AND cloudcoverpercentage:[0 TO {}] ", i)
                }
            }
        })
        .unwrap_or("".to_string());

    let relative_orbit = {
        if m.is_present("RELATIVEORBIT") {
            format!("AND ({})", m.values_of("RELATIVEORBIT")
                .unwrap()
                .map(|s| {
                    match s.parse::<usize>() {
                        Err(_) => panic!("Relative orbit must be integer between 1 and 143!"),
                        Ok(mut i) => {
                            if i < 1 {
                                i = 1;
                            } else if i > 143 {
                                i = 143;
                            }

                            format!("relativeorbitnumber:{}", i)
                        }
                    }
                })
                .collect::<Vec<String>>()
                .join(" OR ")
            )
        } else {
            "".to_string()
        }
    };

    let tile_filter = m.value_of("TILE")
        // TODO: validate, e.g:
        // let mut bytes = tile_filter.as_bytes();

        // if bytes.starts_with(b"T") {
        //     bytes = &bytes[1..];
        // }

        // match bytes {
        //     &[
        //         b'0'..=b'9', b'0'..=b'9',
        //         b'a'..=b'z', b'a'..=b'z', b'a'..=b'z'
        //     ] => {
        //         println!("success!");
        //     }
        //     _ => panic!("invalid input!"),
        // }
        .map(|t| {
            let string = String::from(t);
            let tile_id =
                if string.to_uppercase().starts_with('T') {
                    let utmzone = string.get(1..3).unwrap();
                    let latgrid = string.get(3..6).unwrap();
                    format!("T{}{}", utmzone, latgrid.to_uppercase())
                } else {
                    let utmzone = string.get(0..2).unwrap();
                    let latgrid = string.get(2..5).unwrap();
                    format!("T{}{}", utmzone, latgrid.to_uppercase())
                };
            format!("AND filename: S2?_MSIL{}_*_{}_* ", product_type, tile_id)
        })
        .unwrap_or("".to_string());

    let mut url = reqwest::Url::parse("https://scihub.copernicus.eu/dhus/search")?;
    url.set_scihub_auth(&cfg)?;

    let mut scihub_footprint = wkt_str.trim().clone().to_string();
    // TODO: Refine epsilon
    let mut epsilon = 0.00001;

    loop {
        url.query_pairs_mut().append_pair("rows", "100");
        url.query_pairs_mut().append_pair("orderby", "beginposition asc");
        let querystring = format!("platformname:Sentinel-2 \
                                   AND producttype:S2MSI{} \
                                   AND beginposition:[{} TO {}] \
                                   {}\
                                   {}\
                                   {}\
                                   AND footprint:\"Intersects({})\"",
                                  product_type, begin_time, end_time,
                                  ccover, tile_filter, relative_orbit,
                                  scihub_footprint);
        url.query_pairs_mut().append_pair("q", querystring.as_str());

        if url.as_str().len() < REQUEST_LIMIT {
            break
        }
        scihub_footprint = simplify_polygon(wkt_str.trim(), &epsilon);
        url.query_pairs_mut().clear();
        epsilon *= 1.2;
    }

    if m.is_present("QUERYSTRING") {
        url.query_pairs_mut().append_pair("start", "0");
        println!("{}", url);
        return Ok(())
    }

    if m.is_present("DUMPWKT") {
        let scihub_safe_wkt_filename = m.value_of("DUMPWKT").unwrap();
        dump_wkt(scihub_footprint, scihub_safe_wkt_filename);
    }

    let client = Client::new();
    let total_results = request(url.as_str(), 0, &client).await?;

    let limit = m.value_of("LIMIT")
        .map(|s| s.parse::<u64>().expect("LIMIT must be a positive integer!"))
        .unwrap_or(total_results);

    let responses = stream::iter((100..limit).step_by(100))
        .map(|n| {
            request(url.as_str(), n, &client)
        })
        .buffered(10);
    responses.for_each(|r| {
        async {
            match r {
                Ok(_) => {},
                Err(e) => eprintln!("Got an error: {}", e),
            }
        }
    }).await;

    Ok(())
}

async fn request(url: &str, start: u64,
                 client: &Client) -> Result<u64, Box<dyn std::error::Error>> {

    let mut paginated_url = reqwest::Url::parse(url)?;
    paginated_url.query_pairs_mut().append_pair("start", format!("{}", start).as_str());

    let res = client.get(paginated_url.as_str()).send().await?;
    let status = res.status();

    if status.is_success() {
        let t = &res.text().await?;
        let xml_struct: std::result::Result<Feed, serde_xml_rs::Error>
                        = serde_xml_rs::from_str(&t.as_str());

        match xml_struct {
            Ok(feed) => {
                if feed.total_results > 0 {
                    feed.entry.map(|entries| {
                        for e in entries {
                            println!("{}", e.title);
                        }
                    });
                }
                return Ok(feed.total_results)
            }
            Err(e) => {
                println!("Error parsing response XML! ({})", status.as_u16());
                return Err(e.into())
            }
        }

    }
    else {
        match status.as_u16() {
            400 => panic!("Exceeded scihub max row amount of 100!"),
            // TODO: check if scihub API supports POST
            414 => panic!("Query too large! Probably too long WKT string..."),
            _ => panic!("Invalid response: {}", status)
        }
    }
}

fn manage_config() {
    // Entering new config reads interactive user input from stdin, but when stdin is already used
    // for reading WKT, we cannot ask for user input. This can be detected by checking if the app
    // is running in an interactive shell, but that requires libc which we cannot rely on when
    // Alpine Linux is supported, which uses musl instead of libc. To ensure that stdin is not used
    // for reading WKT, we panic and prompt the user to run the application with the `-s`
    // parameter, which disables WKT file as input to the CLI.

    let cfg: ScihubConfig = confy::load(crate_name!()).unwrap();
    if cfg.username.as_str() == "" || cfg.password.as_str() == "" {
        panic!("No scihub credentials found! Run `scihub-query -s`");
    }

    let new_cfg = ScihubConfig {
        username: input().msg("Enter scihub username: ").get(),
        password: input().msg("Enter scihub password: ").get(),
    };

    confy::store(crate_name!(), new_cfg).unwrap();
    println!("Credentials stored! \
              Subsequent queries will use newly entered credentials..");
}

fn read_creds_from_env() -> Option<ScihubConfig> {
    if let Ok(u) = var("SCIHUB_USER") {
        if let Ok(p) = var("SCIHUB_PASS") {
            return Some(ScihubConfig { username: u, password: p })
        }
    }
    None
}

fn valid_date(s: &str) -> String {
    let parts: Vec<&str> = s.split('-').collect();
    match parts[..] {
        [_, _, _] => {
            return format!("{}T00:00:00.000Z", s);
        },
        // TODO: Proper error
        _ => panic!("Malformed date! Ensure date follows: `YYYY-MM-DD`")
    }
}

fn simplify_polygon(wkt_str: &str, epsilon: &f32) -> String {
    let wkt_roi: wkt::Wkt<f32> = wkt::Wkt::from_str(wkt_str).unwrap();

    if wkt_roi.items.len() != 1 {
        // TODO: Proper error
        panic!("WKT contains more than one object!");
    }

    match try_into_geometry(&wkt_roi.items[0]).unwrap() {
        Geometry::Polygon(p) => {
            let simplified_wkt = Geometry::Polygon(p.simplify(epsilon)).to_wkt();
            return format!("{}", simplified_wkt.items[0]);
        },
        // TODO: Proper error
        _ => panic!("Only Polygon() allowed in WKT!")
    }
}

fn dump_wkt(wkt_str: String, fname: &str) {
    let path = Path::new(fname);
    let display = path.display();

    let mut file = match File::create(&path) {
        Err(why) => {
            eprintln!("couldn't create {}: {}", display, why);
            return
        },
        Ok(file) => file,
    };

    match file.write_all(wkt_str.as_bytes()) {
        Err(why) => eprintln!("couldn't write to {}: {}", display, why),
        Ok(_) => {},
    }
}
