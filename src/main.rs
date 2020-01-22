#[macro_use]
extern crate clap;
extern crate serde;
extern crate serde_derive;
extern crate serde_xml_rs;
extern crate reqwest;
extern crate confy;

use clap::{Arg, App};

use std::io::{self, Read};
use std::fs::File;
use std::{fmt, error::Error};
use read_input::prelude::*;

use serde::{Serialize, Deserialize};

#[derive(Deserialize, Debug)]
struct Feed {
    #[serde(rename = "totalResults")]
    pub total_results: i64,
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

// Specific error types and traits to convert error from output type of Url::set_* output type of
// reqwest::blocking::get() ..

#[derive(Debug)]
enum SciQueryError {
    Credential(())
}

trait UrlCreds {
    fn set_credentials(&mut self, cfg: &ScihubConfig) -> Result<(), SciQueryError>;
}

impl From<()> for SciQueryError {
    fn from(_: ()) -> SciQueryError {
        SciQueryError::Credential(())
    }
}

impl UrlCreds for reqwest::Url {
    fn set_credentials(&mut self, cfg: &ScihubConfig) -> Result<(), SciQueryError> {
        self.set_username(&cfg.username.as_str())?;
        self.set_password(Some(&cfg.password.as_str()))?;
        Ok(())
    }
}

impl fmt::Display for SciQueryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Error when setting credentials!")
    }
}

impl Error for SciQueryError {}

fn main() -> Result<(), Box<dyn Error>> {
    let m = App::new(crate_name!())
        .version(crate_version!())
        .author(crate_authors!())
        .about("Query scihub")
        .arg(Arg::with_name("BEGIN")
            .short("b")
            .long("--begin-time")
            .takes_value(true)
            .required_unless("STORECREDS")
            .help("Date range start (YYYY-MM-DD)"))
        .arg(Arg::with_name("END")
            .short("e")
            .long("--end-time")
            .takes_value(true)
            .default_value("NOW")
            .help("Date range end (YYYY-MM-DD)"))
        .arg(Arg::with_name("PRODUCT")
            .short("p")
            .long("--product-type")
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
            .long("--cloud-cover")
            .takes_value(true)
            .help("Cloud cover percentage. 0 (clear sky) - 100 (complete cover)"))
        .arg(Arg::with_name("STORECREDS")
            .short("s")
            .long("--store-credentials")
            .help("Write new scihub credentials"))
    .get_matches();

    // Entering new config is problematic when reading from stdin if the WKT data is also piped in
    // from stdin. Since checking if the app is running in an interactive shell requires libc,
    // we want to avoid this scenario all toghether as running this app on Alpine linux would use
    // musl instead of libc. Hence the panic when unconfigured and early return to avoid mutating
    // the config struct after setting new values.
    let cfg: ScihubConfig = confy::load(crate_name!())?;
    if cfg.username.as_str() == "" || cfg.password.as_str() == "" {
        panic!("No scihub credentials found! Run `scihub-query -s`");
    }

    if m.is_present("STORECREDS") {
        let new_cfg = ScihubConfig {
            username: input().msg("Enter scihub username: ").get(),
            password: input().msg("Enter scihub password: ").get(),
        };

        confy::store(crate_name!(), new_cfg)?;
        println!("Credentials stored! \
                  Subsequent queries will use newly entered credentials..");
        return Ok(());
    }

    let mut wkt = String::new();

    match m.value_of("WKT").unwrap() {
        "-" => {
            let stdin = io::stdin();
            let mut handle = stdin.lock();
            handle.read_to_string(&mut wkt).expect("Error reading from stdin!");
        },
        filename => {
            let mut f = File::open(filename).expect("file not found");
            f.read_to_string(&mut wkt).expect("Error reading from file!");
        }
    }

    // TODO: support human time format
    let begin_time = format!("{}T00:00:00.000Z", m.value_of("BEGIN").unwrap());

    let end_time = match m.value_of("END").unwrap() {
        "NOW" => "NOW".to_string(),
        dt => format!("{}T00:00:00.000Z", dt)
    };

    let product_type = m.value_of("PRODUCT").unwrap();

    let ccover = match m.value_of("CCOVER") {
        Some(s) => format!("AND cloudcoverpercentage:[0 TO {}] ", s),
        None => "".to_string()
    };

    let mut url = reqwest::Url::parse("https://scihub.copernicus.eu/dhus/search")?;
    url.set_credentials(&cfg)?;
    url.query_pairs_mut().append_pair("rows", "100");
    url.query_pairs_mut().append_pair("orderby", "beginposition asc");
    let querystring = format!("platformname:Sentinel-2 \
                               AND producttype:S2MSI{} \
                               AND beginposition:[{} TO {}] \
                               {}\
                               AND footprint:\"Intersects({})\"",
                              product_type, begin_time, end_time,
                              ccover, wkt.trim());

    url.query_pairs_mut().append_pair("q", querystring.as_str());

    request(url.as_str(), 0)
}

// TODO: check if scihub paginated calls can be async
fn request(url: &str, start: usize) -> Result<(), Box<dyn std::error::Error>> {
    let mut paginated_url = reqwest::Url::parse(url)?;
    paginated_url.query_pairs_mut().append_pair("start", format!("{}", start).as_str());

    let res = reqwest::blocking::get(paginated_url.as_str())?;
    let status = res.status();

    if status.is_success() {
        let t = &res.text()?;
        let xml_struct: std::result::Result<Feed, serde_xml_rs::Error>
                        = serde_xml_rs::from_str(&t.as_str());

        match xml_struct {
            Ok(feed) => {
                if feed.total_results > 0 {
                    match feed.entry {
                        Some(entries) => {
                            for e in entries {
                                println!("{}", e.title);
                            }
                        },
                        None => {
                            return Ok(())
                        }
                    }
                }

                if feed.total_results > 100 {
                    request(url, start + 100)?;
                }
            }
            Err(e) => {
                println!("{}", t);
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

    Ok(())
}
