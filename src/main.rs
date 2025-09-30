use alpm::Alpm;
use alpm::PackageReason;
use serde::{Deserialize, Serialize};
use clap::{Parser, Subcommand};
use std::env;
use std::fs;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::path::Path;
use std::process::{exit, Command, Stdio};
use std::sync::OnceLock;

static ORIGINAL_USER: OnceLock<String> = OnceLock::new();
static USER_DIRECTORY: OnceLock<String> = OnceLock::new();
const SYSTEM_DIRECTORY: &str = "/var/lib/novarch";
const SYSTEM_FILE: &str = "/var/lib/novarch/test.yaml";

fn get_original_user() -> Result<(), String> {
    let user = env::var("SUDO_USER").map_err(|_| "Program must be run with sudo".to_string())?;

    ORIGINAL_USER
        .set(user.clone())
        .map_err(|_| "User already set")?;
    USER_DIRECTORY
        .set(user)
        .map_err(|_| "User directory already set")?;
    Ok(())
}

fn run_command(command: &str) {
    let result = Command::new("sh")
        .arg("-c")
        .arg(command)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status();

    match result {
        Ok(status) => {
            if !status.success() {
                eprintln!("Command failed with exit code: {:?}", status.code());
            }
        }
        Err(e) => {
            eprintln!("Failed to execute command: {}", e);
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct Config {
    folder: String,
    packages: Vec<String>,
}

fn save_systemfile(config: &Config) -> Result<(), Box<dyn std::error::Error>> {
    let file = File::create(SYSTEM_FILE)?;
    let writer = BufWriter::new(file);
    serde_yaml_ng::to_writer(writer, config)?;
    Ok(())
}

fn setup_check() {
    fs::create_dir_all(SYSTEM_DIRECTORY).expect("Failed to create system directory");
    if Path::new(SYSTEM_FILE).exists() {
        let file = File::open(SYSTEM_FILE).expect("Failed to read systemfile");
        let reader = BufReader::new(file);
        let config: Config = serde_yaml_ng::from_reader(reader).expect("Failed to read systemfile");

        if Path::new(&config.folder).is_dir() {
            println!("")
        } else {
            eprintln!("Folder {} does not exist", config.folder)
        }
    } else {
        println!("System file does not exists starting fresh...");
        println!("Enter path for packages folder: ");
        let mut input = String::new();
        io::stdin()
            .read_line(&mut input)
            .expect("Failed to read input");
        let trimed_input = input.trim();
        let folder = match (trimed_input.starts_with("~"), ORIGINAL_USER.get()) {
            (true, Some(user)) => {
                format!("/home/{}{}", user, &trimed_input[1..])
            }
            (true, None) => {
                eprintln!("Script is not ran as sudo");
                trimed_input.to_string()
            }
            (false, _) => trimed_input.to_string(),
        };
        let mut new_config = Config {
            folder: String::new(),
            packages: Vec::new(),
        };
        if Path::new(&folder).is_dir() {
            new_config.folder = folder;
            if let Err(e) = save_systemfile(&new_config) {
                eprintln!("Error saving systemfile {}", e)
            }
        }
    }
}

fn update_system() {
    let output = Command::new("pacman")
        .args(&["-Qi", "reflector"])
        .output()
        .expect("Failed to check for reflector");

    if !output.status.success() {
        println!("Reflector not installed installing now");
        run_command("pacman -S --noconfirm reflector");
    }
    run_command(
        "reflector --latest 10 --protocol https --sort rate --save /etc/pacman.d/mirrorlist",
    );
    run_command("paru -Syu --noconfirm");
}

fn chaotic_aur_setup() {
    let multilib_enabled = match OpenOptions::new().read(true).open("/etc/pacman.conf") {
        Ok(file) => BufReader::new(file)
            .lines()
            .map(|line| line.unwrap_or_default())
            .any(|line| line == "[multilib]"),
        Err(_) => false,
    };
    if !multilib_enabled {
        let mut file = OpenOptions::new()
            .append(true)
            .create(true)
            .open("/etc/pacman.conf")
            .expect("Failed to open file");

        writeln!(file, "[multilib]").expect("Failed to write");
        writeln!(file, "Include = /etc/pacman.d/mirrorlist").expect("Failed to write");
    }

    let chaotic_enabled = match OpenOptions::new().read(true).open("/etc/pacman.conf") {
        Ok(file) => BufReader::new(file)
            .lines()
            .map(|line| line.unwrap_or_default())
            .any(|line| line == "[chaotic-aur]"),
        Err(_) => false,
    };
    if !chaotic_enabled {
        update_system();
        run_command("pacman-key --init");
        run_command("pacman -Sy --noconfirm archlinux-keyring");
        run_command("pacman-key --recv-key 3056513887B78AEB --keyserver keyserver.ubuntu.com");
        run_command("pacman-key --lsign-key 3056513887B78AEB");
        run_command(
            "pacman -U --noconfirm 'https://cdn-mirror.chaotic.cx/chaotic-aur/chaotic-keyring.pkg.tar.zst'"
        );
        run_command(
            "pacman -U --noconfirm 'https://cdn-mirror.chaotic.cx/chaotic-aur/chaotic-mirrorlist.pkg.tar.zst'"
        );
        let mut file = OpenOptions::new()
            .append(true)
            .create(true)
            .open("/etc/pacman.conf")
            .expect("Failed to open file");

        writeln!(file, "[chaotic-aur]").expect("Failed to write");
        writeln!(file, "Include = /etc/pacman.d/chaotic-mirrorlist").expect("Failed to write");
        run_command("pacman -Syu --noconfirm");
    }
}

fn get_system() -> Result<(Vec<String>, Vec<String>, Vec<String>), Box<dyn std::error::Error>> {
    let alpm = Alpm::new("/", "/var/lib/pacman").expect("Failed to read Database");
    let db = alpm.localdb();
    let packages_installed: Vec<String> =
        db.pkgs().iter().map(|pkg| pkg.name().to_string()).collect();
    let file = File::open(SYSTEM_FILE).expect("Failed to read systemfile");
    let reader = BufReader::new(file);
    let config: Config = serde_yaml_ng::from_reader(reader).expect("Failed to read systemfile");
    let existing_packages = config.packages;
    let folder = config.folder;

    let mut all_packages = Vec::new();
    for entry in fs::read_dir(folder)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_file() {
            if let Some(filename) = path.file_name() {
                if let Some(filename_str) = filename.to_str() {
                    if filename_str.ends_with(".yaml") {
                        let file = File::open(path)?;
                        let reader = BufReader::new(file);
                        let packages: Vec<String> = serde_yaml_ng::from_reader(reader)?;
                        all_packages.extend(packages)
                    }
                }
            }
        }
    }

    Ok((packages_installed, all_packages, existing_packages))
}

fn install_packages() {
    let packages_selected;
    let mut existing_packages;
    let user = ORIGINAL_USER.get().unwrap();
    let mut install_command = format!("sudo -u {} paru -S --needed --noconfirm -- ", user);

    match get_system() {
        Ok((_packages_installed, selected, existing)) => {
            packages_selected = selected;
            existing_packages = existing;
        }
        Err(e) => {
            eprintln!("Error reading packages :: {}", e);
            return; // Exit early on error
        }
    }

    let tobe_installed: Vec<String> = packages_selected
        .into_iter()
        .filter(|item| !existing_packages.contains(item))
        .collect();

    if !tobe_installed.is_empty() {
        let max_attempts = 3;
        let mut attempts = 0;
        install_command.push_str(&tobe_installed.join(" "));
        println!("Packages to install :\n{:?}", (&tobe_installed));
        println!("Do you want to proceed installing above packages [Y/n] : ");
        let mut confirmation = String::new();
        io::stdin()
            .read_line(&mut confirmation)
            .expect("Failed to read input");
        let conf = confirmation.trim().to_lowercase();
        if conf == "y" || conf == " " || conf == "" {
            loop {
                attempts += 1;

                let result = Command::new("sh")
                    .arg("-c")
                    .arg(&install_command)
                    .stdout(Stdio::inherit())
                    .stderr(Stdio::inherit())
                    .status();

                match result {
                    Ok(status) => {
                        if status.success() {
                            let file = File::open(SYSTEM_FILE).expect("Failed to read systemfile");
                            let reader = BufReader::new(file);
                            let mut config: Config = serde_yaml_ng::from_reader(reader)
                                .expect("Failed to read systemfile");
                            existing_packages.extend(tobe_installed);
                            config.packages = existing_packages;
                            match save_systemfile(&config) {
                                Ok(_) => {}
                                Err(_) => eprintln!("Failed to save systemfile"),
                            }
                            break;
                        } else {
                            eprintln!(
                                "Command failed with exit code: {:?} (attempt {})",
                                status.code(),
                                attempts
                            );
                            if attempts >= max_attempts {
                                eprintln!("Command failed after {} attempts", max_attempts);
                                break;
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("Failed to execute command: {} (attempt {})", e, attempts);
                        if attempts >= max_attempts {
                            eprintln!("Command execution failed after {} attempts", max_attempts);
                            break;
                        }
                    }
                }
            }
        }
    } else {
        println!("No package to install");
    }
}

fn remove_packages() {
    let packages_selected;
    let packages_installed;
    let mut existing_packages;
    let mut remove_command = format!("pacman -Rns --noconfirm  ");

    match get_system() {
        Ok((installed, selected, existing)) => {
            packages_selected = selected;
            existing_packages = existing;
            packages_installed = installed;
        }
        Err(e) => {
            eprintln!("Error reading packages :: {}", e);
            return; // Exit early on error
        }
    }

    let mut tobe_removed: Vec<String> = existing_packages
        .clone()
        .into_iter()
        .filter(|item| !packages_selected.contains(item))
        .filter(|item| packages_installed.contains(item))
        .collect();

    let alpm = Alpm::new("/", "/var/lib/pacman").expect("Database not found");
    let db = alpm.localdb();

    tobe_removed.retain(|package| match db.pkg(package.to_string()) {
        Ok(pkg) => pkg.required_by().is_empty(),
        Err(_) => false,
    });

    if !tobe_removed.is_empty() {
        remove_command.push_str(&tobe_removed.join(" "));
        println!("Packages to install :\n{:?}", (&tobe_removed));
        println!("Do you want to proceed removing above packages [Y/n] : ");
        let mut confirmation = String::new();
        io::stdin()
            .read_line(&mut confirmation)
            .expect("Failed to read input");
        let conf = confirmation.trim().to_lowercase();
        if conf == "y" || conf == " " || conf == "" {
            let _result = Command::new("sh")
                .arg("-c")
                .arg(&remove_command)
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .status();
        }

        existing_packages.retain(|item| !tobe_removed.contains(item));
        let file = File::open(SYSTEM_FILE).expect("Failed to read systemfile");
        let reader = BufReader::new(file);
        let mut config: Config =
            serde_yaml_ng::from_reader(reader).expect("Failed to read systemfile");
        config.packages = existing_packages;
        match save_systemfile(&config) {
            Ok(_) => {}
            Err(_) => eprintln!("Failed to save systemfile"),
        }
    } else {
        println!("No package to remove");
    }
}

fn manage_package() {
    let output = Command::new("pacman")
        .args(&["-Qi", "paru"])
        .output()
        .expect("Failed to check for reflector");

    if !output.status.success() {
        println!("Paru not installed installing now");
        run_command("pacman -S --noconfirm paru");
    }
    install_packages();
    remove_packages();
}

fn initialize() {
    setup_check();
    chaotic_aur_setup();
    update_system();
    manage_package();
}

fn update() {
    setup_check();
    update_system();
    manage_package();

    let alpm = Alpm::new("/", "/var/lib/pacman").expect("Failed to read database");
    let db = alpm.localdb();

    let orphans: Vec<String> = db
        .pkgs()
        .iter()
        .filter(|pkg| pkg.reason() == PackageReason::Depend && pkg.required_by().is_empty())
        .map(|pkg| pkg.name().to_string())
        .collect();
    if !orphans.is_empty() {
        run_command("pacman -Rns $(pacman -Qdtq)");
    }
}

fn info() {
    let file = File::open(SYSTEM_FILE).expect("Failed to read systemfile");
    let reader = BufReader::new(file);
    let config: Config = serde_yaml_ng::from_reader(reader).expect("Failed to read systemfile");
    let no_of_packages = config.packages.len();
    let folder = config.folder;
    println!("\nPackages folder : {}", folder);
    println!("No of packages installed : {}", no_of_packages)
}

#[derive(Parser)]
#[clap(name = "Declarative Arch", about = "Tool to manage archlinux packages declaratively")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    #[command(name = "init")]
    Init,
    #[command(name = "install")]
    Install,
    #[command(name = "update")]
    Update,
    #[command(name = "info")]
    Info,
}

fn main() {
    match get_original_user() {
        Ok(_user) => {}
        Err(e) => {
            eprintln!("Error: {}", e);
            exit(1);
        }
    }

    let cli = Cli::parse();
    match &cli.command {
        Commands::Init => {
            println!("Initializing...");
            initialize();
        }
        Commands::Install => {
            println!("Installing...");
            manage_package();
        }
        Commands::Update => {
            println!("Updating...");
            update();
        }
        Commands::Info => {
            println!("Showing info...");
            info();
        }
    }
}
