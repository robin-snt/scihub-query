#[macro_use]
extern crate clap;
extern crate serde;
extern crate serde_derive;
extern crate serde_xml_rs;
extern crate reqwest;
extern crate confy;
extern crate tokio;

use clap::{Arg, App};

use std::io::{self, Read};
use std::fs::File;
use std::{fmt, error::Error, env::var};
use read_input::prelude::*;

use serde::{Serialize, Deserialize};
use futures::{stream, StreamExt};
use reqwest::Client;

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

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
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
        .arg(Arg::with_name("LIMIT")
            .short("l")
            .long("--limit")
            .help("Limit"))
        .arg(Arg::with_name("QUERYSTRING")
            .long("--query-string")
            .help("Print scihub query string and exit"))
    .get_matches();

    if m.is_present("STORECREDS") {
        manage_config();
        return Ok(())
    }

    let cfg: ScihubConfig = match read_creds_from_env() {
        Some(d) => d,
        None => confy::load(crate_name!())?
    };

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
        None => "".to_string(),
        Some(s) => {
            match s.parse::<usize>() {
                Err(_) => panic!("Cloud cover must be integer between 0 and 100!"),
                Ok(i) => {
                    if i > 100 {
                        panic!("Cloud cover must be integer between 0 and 100!")
                    }

                    format!("AND cloudcoverpercentage:[0 TO {}] ", i)
                }
            }
        }
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

    if m.is_present("QUERYSTRING") {
        url.query_pairs_mut().append_pair("start", format!("{}", 0).as_str());
        println!("{}", url);
        return Ok(())
    }

    let client = Client::new();
    let total_results = request(url.as_str(), 0, &client).await?;

    let limit = match m.value_of("LIMIT") {
        None => total_results,
        Some(s) => {
            match s.parse::<u64>() {
                Err(_) => panic!("Limit value must be a positive integer!"),
                Ok(i) => {
                    if i > total_results {
                        total_results
                    } else {
                        i
                    }
                }
            }
        }
    };

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

async fn request(url: &str, start: u64, client: &Client) -> Result<u64, Box<dyn std::error::Error>> {
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
                    match feed.entry {
                        Some(entries) => {
                            for e in entries {
                                println!("{}", e.title);
                            }
                        },
                        None => {}
                    }
                }
                return Ok(feed.total_results)
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
}

fn manage_config() {
    // Entering new config is problematic when reading from stdin f.ex if the WKT data is piped
    // from stdin. Since checking if the app is running in an interactive shell requires libc, we
    // want to avoid this scenario all toghether as running this app on Alpine linux would use musl
    // instead of libc. Hence the panic when unconfigured and early return to avoid mutating the
    // config struct after setting new values.
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
