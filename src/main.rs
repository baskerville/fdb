#![recursion_limit = "1024"]

#[macro_use]
extern crate error_chain;
#[macro_use]
extern crate serde_derive;
extern crate bincode;
extern crate time;
extern crate getopts;
extern crate regex;

mod errors {
    error_chain!{}
}

use std::io::prelude::*;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;
use std::str::FromStr;
use std::cmp::Ordering;
use std::io::ErrorKind;
use std::io::stdout;
use std::env;
use std::fs;
use std::fs::File;
use bincode::{serialize_into, deserialize_from, Infinite};
use time::get_time;
use getopts::Options;
use regex::Regex;
use errors::*;

#[derive(Serialize, Deserialize, PartialEq, Debug)]
struct Item {
    path: String,
    atime: i64, // unix time of last access
    hits: u32,
}

struct Settings {
    history_size: usize,
    db_path: String,
    sort_by: SortBy,
}

enum Action {
    Query,
    Add,
    Delete,
}

enum SortBy {
    Frecency,
    Atime,
    Hits,
}

struct Lock(PathBuf);

impl Lock {
    pub fn new(path: &str) -> Result<Lock> {
        let path = PathBuf::from(format!("{}.lock", path));
        while path.exists() {
            thread::sleep(Duration::from_millis(30));
        }
        File::create(&path).chain_err(
            || "Can't create the lock file",
        )?;
        Ok(Lock(path))
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

impl Item {
    fn new(path: &str) -> Item {
        Item {
            path: path.to_string(),
            atime: get_time().sec,
            hits: 1,
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
    env::var(key)
        .ok()
        .and_then(|val| val.parse::<T>().ok())
        .unwrap_or(default)
}

fn parse_sort_method(name: &str) -> Option<SortBy> {
    match name {
        "frecency" => Some(SortBy::Frecency),
        "atime" => Some(SortBy::Atime),
        "hits" => Some(SortBy::Hits),
        _ => None,
    }
}

fn print_version() {
    println!("{}", option_env!("CARGO_PKG_VERSION").unwrap_or("Unknown"));
}

fn print_usage(opts: &Options) {
    println!(
        "{}",
        opts.usage("Usage: fdb [-i DB_PATH] [-u] [-s SORT_BY] -h|-v|-z|-q PATTERN ...|-a PATH ...|-d PATH ...")
    );
}

fn load_data(path: &str) -> Result<Vec<Item>> {
    let mut f = File::open(path).chain_err(|| "Can't open data file")?;
    deserialize_from(&mut f, Infinite).chain_err(|| "Can't deserialize data")
}

fn save_data(data: &Vec<Item>, path: &str) -> Result<()> {
    let new_path = path.to_string() + ".tmp";
    let mut file = File::create(&new_path).chain_err(
        || "Can't create temporary database file",
    )?;
    serialize_into(&mut file, data, Infinite).chain_err(
        || "Can't serialize data into the database",
    )?;
    file.flush().chain_err(
        || "Couldn't flush temporary database file",
    )?;
    fs::rename(new_path, path).chain_err(
        || "Couldn't rename temporary data file",
    )?;
    Ok(())
}

fn cmd_sort(sort_by: SortBy, data: &mut Vec<Item>) {
    match sort_by {
        SortBy::Frecency => data.sort_by(sort_method_frecency),
        SortBy::Atime => data.sort_by(|a, b| a.atime.cmp(&b.atime).reverse()),
        SortBy::Hits => data.sort_by(|a, b| a.hits.cmp(&b.hits).reverse()),
    }
}

fn sort_method_frecency(a: &Item, b: &Item) -> Ordering {
    a.frecency()
        .partial_cmp(&b.frecency())
        .unwrap_or(Ordering::Equal)
        .reverse()
}

fn cmd_add(settings: &Settings, data: &mut Vec<Item>, paths: &Vec<String>) {
    for path in paths.iter() {
        {
            let existing: Option<&mut Item> = data.iter_mut().find(|ref a| a.path == *path);
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

fn cmd_query(sort_by: SortBy, data: &mut Vec<Item>, pattern: &str) -> Result<()> {
    let re = Regex::new(pattern).chain_err(
        || "Couldn't create query regex",
    )?;
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

quick_main!(run);

fn run() -> Result<()> {
    let mut settings = Settings {
        history_size: 600,
        db_path: "~/.z".to_string(),
        sort_by: SortBy::Frecency,
    };

    let args: Vec<String> = env::args().skip(1).collect();
    let mut action: Option<Action> = None;
    let mut opts = Options::new();

    opts.optflag("q", "query", "Query for patterns in the database.");
    opts.optflag("a", "add", "Add paths to the database.");
    opts.optflag("d", "delete", "Delete paths from the database.");
    opts.optflag("u", "unlimited", "Don't limit the size of the database.");
    opts.optflag("z", "initialize", "Initialize the database.");
    opts.optflag("h", "help", "Print this help message.");
    opts.optflag("v", "version", "Print the version number.");
    opts.optopt("i", "db-path", "Use the given database.", "DB_PATH");
    opts.optopt(
        "s",
        "sort-by",
        "Use the given sort method.",
        "frecency|atime|hits",
    );

    let matches = opts.parse(&args).chain_err(
        || "Failed to parse the command line options",
    )?;

    let home_dir = env::home_dir();
    let home_dir = home_dir.as_ref().and_then(|a| a.to_str()).chain_err(
        || "Can't retreive home directory",
    )?;

    settings.db_path = get_env::<String>("FDB_DB_PATH", settings.db_path);
    settings.db_path = matches.opt_str("i").unwrap_or(settings.db_path);
    settings.db_path = settings.db_path.replace("~", home_dir);
    settings.sort_by = matches
        .opt_str("s")
        .and_then(|name| parse_sort_method(&name))
        .unwrap_or(settings.sort_by);
    settings.history_size = get_env::<usize>("FDB_HISTORY_SIZE", settings.history_size);

    if matches.opt_present("u") {
        settings.history_size = 0;
    }

    if matches.opt_present("z") {
        return save_data(&vec![], &settings.db_path).chain_err(|| "Can't initialize data");
    } else if matches.opt_present("h") {
        print_usage(&opts);
        return Ok(());
    } else if matches.opt_present("v") {
        print_version();
        return Ok(());
    }

    let lock = Lock::new(&settings.db_path).chain_err(
        || "Can't lock database",
    )?;

    if matches.opt_present("q") {
        action = Some(Action::Query);
    } else if matches.opt_present("a") {
        action = Some(Action::Add);
    } else if matches.opt_present("d") {
        action = Some(Action::Delete);
    }

    if action.is_none() || matches.free.len() < 1 {
        print_usage(&opts);
        return Ok(());
    }

    let action = action.unwrap();
    let mut data: Vec<Item> = load_data(&settings.db_path).chain_err(|| "Can't load data")?;

    match action {
        Action::Add => cmd_add(&settings, &mut data, &matches.free),
        Action::Delete => cmd_delete(&mut data, &matches.free),
        Action::Query => {
            return cmd_query(settings.sort_by, &mut data, &matches.free.join(".*"))
                .chain_err(|| "Can't execute query")
        }
    }

    save_data(&data, &settings.db_path).chain_err(
        || "Can't save data",
    )?;

    drop(lock);
    Ok(())
}
