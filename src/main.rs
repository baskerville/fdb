extern crate rustc_serialize;
extern crate time;
extern crate getopts;
extern crate regex;

use std::io::prelude::*;
use std::env;
use std::fs;
use std::fs::File;
use rustc_serialize::json;
use time::get_time;
use getopts::Options;
use regex::Regex;
use std::str::FromStr;
use std::cmp::Ordering;
use std::io::ErrorKind;
use std::io::stdout as stdout;
use std::io::Error as IoError;
use regex::Error as RegexError;

#[derive(Debug, Clone, RustcEncodable, RustcDecodable)]
struct Item {
    path: String,
    // unix time of last access
    atime: i64,
    hits: u32
}

struct Settings {
    history_size: usize,
    db_path: String,
    sort_by: SortBy
}

enum Action {
    Query,
    Add,
    Delete
}

enum SortBy {
    Frecency,
    Atime,
    Hits
}

impl Item {
    fn new(path: &str) -> Item {
        Item {
            path: path.to_string(),
            atime: get_time().sec,
            hits: 1
        }
    }

    fn frecency(&self) -> f32 {
        let age = (get_time().sec - self.atime) as f32;
        (self.hits as f32) / (0.25 + 3e-6 * age)
    }

    fn touch(&mut self) {
        self.hits += 1;
        self.atime = get_time().sec;
    }
}

fn get_env<T: FromStr>(key: &str, default: T) -> T {
    env::var(key).ok().and_then(|val| val.parse::<T>().ok()).unwrap_or(default)
}

fn parse_sort_method(name: &str) -> Option<SortBy> {
    match name {
        "frecency" => Some(SortBy::Frecency),
        "atime" => Some(SortBy::Atime),
        "hits" => Some(SortBy::Hits),
        _ => None
    }
}

#[derive(Debug)]
enum Error {
    Io(IoError),
    Decode(json::DecoderError),
    Encode(json::EncoderError),
    Regex(RegexError)
}

impl From<IoError> for Error {
    fn from(err: IoError) -> Error {
        Error::Io(err)
    }
}

impl From<json::DecoderError> for Error {
    fn from(err: json::DecoderError) -> Error {
        Error::Decode(err)
    }
}

impl From<json::EncoderError> for Error {
    fn from(err: json::EncoderError) -> Error {
        Error::Encode(err)
    }
}

impl From<RegexError> for Error {
    fn from(err: RegexError) -> Error {
        Error::Regex(err)
    }
}

fn print_version() {
    println!("{}", option_env!("CARGO_PKG_VERSION").unwrap_or("Unknown"));
}

fn print_usage(opts: &Options) {
    println!("{}", opts.usage("Usage: fdb -h|-v|-q|-a|-d [-i DB_PATH] PATTERN...|PATH..."));
}

fn load_data(path: &str) -> Result<Vec<Item>, Error> {
    let mut f = try!(File::open(path));
    let mut s = String::new();
    try!(f.read_to_string(&mut s));
    let v: Vec<Item> = try!(json::decode(&s));
    Ok(v)
}

fn save_data(data: &Vec<Item>, path: &str) -> Result<(), Error> {
    let np = path.to_string() + ".new";
    let mut f = try!(File::create(&np));
    let j = try!(json::encode(data));
    try!(f.write(j.as_bytes()));
    try!(fs::rename(np, path));
    Ok(())
}

fn cmd_sort(sort_by: SortBy, data: &mut Vec<Item>) {
    match sort_by {
        SortBy::Frecency => data.sort_by(|a, b| a.frecency().partial_cmp(&b.frecency()).unwrap_or(Ordering::Equal).reverse()),
        SortBy::Atime => data.sort_by(|a, b| a.atime.cmp(&b.atime).reverse()),
        SortBy::Hits => data.sort_by(|a, b| a.hits.cmp(&b.hits).reverse())
    }
}

fn cmd_add(settings: &Settings, data: &mut Vec<Item>, paths: &Vec<String>) {
    for path in paths.iter() {
        {
            let existing:Option<&mut Item> = data.iter_mut().find(|ref a| a.path == *path);
            if existing.is_some() {
                existing.unwrap().touch();
                continue;
            }
        }
        data.push(Item::new(&path));
    }
    if settings.history_size > 0 && data.len() > settings.history_size {
        cmd_sort(SortBy::Frecency, data);
        while data.len() > settings.history_size {
            data.pop();
        }
    }
}

fn cmd_delete(data: &mut Vec<Item>, paths: &Vec<String>) {
    data.retain(|ref a| paths.iter().find(|&p| a.path == *p).is_none());
}

fn cmd_query(sort_by: SortBy, data: &mut Vec<Item>, pattern: &str) -> Result<(), Error> {
    let re = try!(Regex::new(pattern));
    let mut stdout = stdout();
    cmd_sort(sort_by, data);
    for item in data.iter() {
        if re.is_match(&item.path) {
            // avoid panicking on `fdb -q PATTERN | head -n 1`
            if let Err(e) = write!(&mut stdout, "{}\n", item.path) {
                if e.kind() == ErrorKind::BrokenPipe {
                    break;
                } else {
                    panic!("Couldn't write to stdout: {:?}.", e);
                }
            }
        }
    }
    Ok(())
}

fn main() {
    let mut settings = Settings {
        history_size: 600,
        db_path: "~/.z.json".to_string(),
        sort_by: SortBy::Frecency
    };

    let args: Vec<String> = env::args().skip(1).collect();
    let mut action: Option<Action> = None;
    let mut opts = Options::new();

    opts.optflag("q", "query", "Query for patterns in the database.");
    opts.optflag("a", "add", "Add paths to the database.");
    opts.optflag("d", "delete", "Delete paths from the database.");
    opts.optflag("u", "unlimited", "Don't limit the size of the database.");
    opts.optflag("h", "help", "Print this help message.");
    opts.optflag("v", "version", "Print the version number.");
    opts.optopt("i", "db-path", "Use the given database.", "DB_PATH");
    opts.optopt("s", "sort-by", "Use the given sort method.", "frecency|atime|hits");

    let matches = match opts.parse(&args) {
        Ok(m) => m,
        Err(e) => panic!("Failed to parse the command line options: {:?}.", e)
    };

    if matches.opt_present("q") {
       action = Some(Action::Query);
    } else if matches.opt_present("a") {
        action = Some(Action::Add);
    } else if matches.opt_present("d") {
        action = Some(Action::Delete);
    } else if matches.opt_present("h") {
        print_usage(&opts);
        return;
    } else if matches.opt_present("v") {
        print_version();
        return;
    }

    if action.is_none() || matches.free.len() < 1 {
        print_usage(&opts);
        return;
    }

    let home_dir = env::home_dir();
    let home_dir = match home_dir.as_ref().and_then(|a| a.to_str()) {
        Some(val) => val,
        None => panic!("Can't retreive home directory.")
    };

    settings.db_path = get_env::<String>("FDB_DB_PATH", settings.db_path);
    settings.db_path = matches.opt_str("i").unwrap_or(settings.db_path);
    settings.db_path = settings.db_path.replace("~", home_dir);
    settings.sort_by = matches.opt_str("s").and_then(|name| parse_sort_method(&name)).unwrap_or(settings.sort_by);
    settings.history_size = get_env::<usize>("FDB_HISTORY_SIZE", settings.history_size);

    if matches.opt_present("u") {
        settings.history_size = 0;
    }

    let mut data: Vec<Item> = match load_data(&settings.db_path) {
        Ok(val) => val,
        Err(e) => panic!("Can't load data: {:?}.", e)
    };

    match action {
        Some(Action::Add) => cmd_add(&settings, &mut data, &matches.free),
        Some(Action::Delete) => cmd_delete(&mut data, &matches.free),
        Some(Action::Query) => {
            match cmd_query(settings.sort_by, &mut data, &matches.free.join(".*")) {
                Err(e) => panic!("Can't parse query pattern: {:?}.", e),
                _ => return
            }
        }
        None => unreachable!()
    }

    if let Err(e) = save_data(&data, &settings.db_path) {
        panic!("Can't save data: {:?}.", e);
    }
}
