#[macro_use] extern crate failure;
#[macro_use] extern crate serde_derive;
extern crate bincode;
extern crate time;
extern crate getopts;
extern crate regex;

use std::io::prelude::*;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;
use std::str::FromStr;
use std::cmp::Ordering;
use std::io::ErrorKind;
use std::io::stdout;
use std::process;
use std::env;
use std::fs;
use std::fs::OpenOptions;
use std::fs::File;
use bincode::{serialize_into, deserialize_from};
use time::get_time;
use getopts::Options;
use regex::Regex;
use failure::{Error, ResultExt};

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

#[derive(Copy, Clone)]
enum Action {
    Query,
    Add,
    Delete,
}

#[derive(Copy, Clone)]
enum SortBy {
    Frecency,
    Atime,
    Hits,
}

struct Lock(PathBuf);

impl Lock {
    pub fn new(path: &str) -> Result<Lock, Error> {
        let path = PathBuf::from(format!("{}.lock", path));
        while path.exists() {
            thread::sleep(Duration::from_millis(30));
        }
        let mut file = OpenOptions::new().write(true).create_new(true).open(&path);
        while let Err(e) = file {
            if e.kind() == ErrorKind::AlreadyExists {
                while path.exists() {
                    thread::sleep(Duration::from_millis(30));
                }
            } else {
                return Err(Error::from(e).context("Can't create the lock file").into());
            }
            file = OpenOptions::new().write(true).create_new(true).open(&path);
        }
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

fn load_data(path: &str) -> Result<Vec<Item>, Error> {
    let mut f = File::open(path).context("Can't open data file")?;
    deserialize_from(&mut f).context("Can't deserialize data").map_err(Into::into)
}

fn save_data(data: &[Item], path: &str) -> Result<(), Error> {
    let new_path = path.to_string() + ".tmp";
    let mut file = File::create(&new_path)?;
    serialize_into(&mut file, data).context("Can't serialize data into the database")?;
    file.flush().context("Couldn't flush temporary database file")?;
    fs::rename(new_path, path).context("Couldn't rename temporary data file").map_err(Into::into)
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

fn cmd_add(settings: &Settings, data: &mut Vec<Item>, paths: &[String]) {
    for path in paths.iter() {
        {
            let existing: Option<&mut Item> = data.iter_mut().find(|a| a.path == *path);
            if existing.is_some() {
                existing.unwrap().touch();
                continue;
            }
        }
        data.push(Item::new(path));
    }
    if settings.history_size > 0 && data.len() > settings.history_size {
        cmd_sort(SortBy::Frecency, data);
        while data.len() > settings.history_size {
            data.pop();
        }
    }
}

fn cmd_delete(data: &mut Vec<Item>, paths: &[String]) {
    data.retain(|a| paths.iter().find(|&p| a.path == *p).is_none());
}

fn cmd_query(sort_by: SortBy, data: &mut Vec<Item>, pattern: &str) -> Result<(), Error> {
    let re = Regex::new(pattern).context("Couldn't create query regex")?;
    let mut stdout = stdout();
    cmd_sort(sort_by, data);
    for item in data.iter() {
        if re.is_match(&item.path) {
            // avoid panicking on `fdb -q PATTERN | head -n 1`
            if let Err(e) = writeln!(&mut stdout, "{}", item.path) {
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

fn run() -> Result<(), Error> {
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

    let matches = opts.parse(&args).context("Failed to parse the command line options")?;

    let home_dir = env::home_dir();
    let home_dir = home_dir.as_ref().and_then(|a| a.to_str()).ok_or_else(|| format_err!("Can't retreive home directory"))?;

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
        return save_data(&[], &settings.db_path).context("Can't initialize data").map_err(Into::into);
    } else if matches.opt_present("h") {
        print_usage(&opts);
        return Ok(());
    } else if matches.opt_present("v") {
        print_version();
        return Ok(());
    }

    let lock = Lock::new(&settings.db_path).context("Can't lock database")?;

    if matches.opt_present("q") {
        action = Some(Action::Query);
    } else if matches.opt_present("a") {
        action = Some(Action::Add);
    } else if matches.opt_present("d") {
        action = Some(Action::Delete);
    }

    if action.is_none() || matches.free.is_empty() {
        print_usage(&opts);
        return Ok(());
    }

    let action = action.unwrap();
    let mut data: Vec<Item> = load_data(&settings.db_path).context("Can't load data")?;

    match action {
        Action::Add => cmd_add(&settings, &mut data, &matches.free),
        Action::Delete => cmd_delete(&mut data, &matches.free),
        Action::Query => {
            return cmd_query(settings.sort_by, &mut data, &matches.free.join(".*"))
                .context("Can't execute query").map_err(Into::into)
        }
    }

    save_data(&data, &settings.db_path).context("Can't save data")?;

    drop(lock);
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        for e in e.causes() {
            eprintln!("fdb: {}.", e);
        }
        process::exit(1);
    }
}
