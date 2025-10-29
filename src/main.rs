use alpm::Alpm;
use alpm::PackageReason;
use clap::{Parser, Subcommand};
use const_format::concatcp;
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::path::Path;
use std::path::PathBuf;
use std::process::{Command, Stdio, exit};
use std::sync::OnceLock;
use std::thread;
use std::time::Duration;
use rustyline::completion::FilenameCompleter;
use rustyline::Editor;


static ORIGINAL_USER: OnceLock<String> = OnceLock::new();
static USER_DIRECTORY: OnceLock<String> = OnceLock::new();
const SYSTEM_DIRECTORY: &str = "/var/lib/novarch";
const SYSTEM_FILE: &str = "/var/lib/novarch/system.yaml";

// ANSI color codes
const RESET: &str = "\x1b[0m";
const RED_CROSS: &str = concatcp!("\x1b[91m", "✗", RESET);
const YELLOW_WARNING: &str = concatcp!("\x1b[93m", "⚠", RESET);
const BLUE_GEAR: &str = concatcp!("\x1b[94m", "⚙", RESET);
const GREEN_CHECK: &str = concatcp!("\x1b[92m", "✓", RESET);

fn get_original_user() -> Result<(), String> {
    let user = env::var("USER")
        .or_else(|_| env::var("LOGNAME"))
        .map_err(|_| "Could not determine current user".to_string())?;

    ORIGINAL_USER
        .set(user.clone())
        .map_err(|_| "User already set")?;
    USER_DIRECTORY
        .set(user)
        .map_err(|_| "User directory already set")?;
    Ok(())
}

fn ask_confirmation(message: &str) -> bool {
    print!("{}", message);
    io::stdout().flush().expect("Failed to flush stdout");
    
    let mut confirmation = String::new();
    io::stdin()
        .read_line(&mut confirmation)
        .expect("Failed to read input");
    
    let conf = confirmation.trim().to_lowercase();
    conf == "y" || conf == " " || conf == ""
}


fn is_network_or_download_error(error_output: &str) -> bool {
    let error_lower = error_output.to_lowercase();
    
    let network_patterns = [
        "failed retrieving file",
        "failed to download",
        "download failed",
        "connection timed out",
        "connection refused",
        "could not resolve host",
        "temporary failure in name resolution",
        "network is unreachable",
        "curl error",
        "timeout",
        "ssl",
        "tls",
        "certificate",
        "failed to retrieve",
        "error: target not found",  // Sometimes network related
        "could not connect",
        "no route to host",
        "http error 404",
        "http error 503",
        "http error 502",
    ];
    
    network_patterns.iter().any(|pattern| error_lower.contains(pattern))
}

fn run_command(command: &str, needs_sudo: bool) -> bool {
    let final_command = if needs_sudo {
        format!("sudo {}", command)
    } else {
        command.to_string()
    };

    let max_attempts = 3;
    let mut attempts = 0;

    let out = Command::new("sh")
        .arg("-c")
        .arg(&final_command)
        .stdout(Stdio::inherit())
        .stdin(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status();

    match out {
        Ok(status) => {
            if status.success() {
                return true;
            }else {
                if !ask_confirmation("Error occured do you want to retry [Y/n] : "){
                    std::process::exit(1);
                }
            }
        }
        Err(e) => {
            eprintln!("{} Failed to run command {}\n Error:{}", RED_CROSS, final_command, e)
        }
    }
    loop {
        attempts += 1;

        let result = Command::new("sh")
            .arg("-c")
            .arg(&final_command)
            .stdout(Stdio::inherit())
            .stdin(Stdio::inherit())
            .stderr(Stdio::piped())
            .output();

        match result {
            Ok(output) => {
                let stderr_output = String::from_utf8_lossy(&output.stderr);
                
                if output.status.success() {
                    return true;
                } else {
                    eprint!("{}", stderr_output);
                    
                    if is_network_or_download_error(&stderr_output) {
                        eprintln!(
                            "\n{} Network/Download error detected (attempt {})",
                            YELLOW_WARNING, attempts
                        );
                        if attempts >= max_attempts {
                            eprintln!(
                                "{} Installation failed after {} attempts due to network issues",
                                RED_CROSS, max_attempts
                            );
                            std::process::exit(1);
                        } else {
                            let duration = attempts*5;
                            thread::sleep(Duration::from_secs(duration));
                        }
                    } else {
                        if attempts >= max_attempts {
                            std::process::exit(1)
                        }else {
                            if !ask_confirmation("Error occured do you want to retry [Y/n] : "){
                                std::process::exit(1);
                            }
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("{} Command execution failed: {}\n {}", RED_CROSS, final_command, e);
                std::process::exit(1);
            }
        }
    }
}


#[derive(Debug, Deserialize, Serialize)]
struct Config {
    folder: String,
    packages: Vec<String>,
}

fn save_systemfile(config: &Config) -> Result<(), Box<dyn std::error::Error>> {
    let yaml_content = serde_yaml_ng::to_string(config)?;

    let mut child = Command::new("sudo")
        .args(&["tee", SYSTEM_FILE])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(yaml_content.as_bytes())?;
    }

    let status = child.wait()?;
    if !status.success() {
        return Err("Failed to write system file".into());
    }

    Ok(())
}

fn ensure_system_directory() {
    let result = Command::new("sudo")
        .args(&["mkdir", "-p", SYSTEM_DIRECTORY])
        .status();

    match result {
        Ok(status) => {
            if !status.success() {
                eprintln!("{} Failed to create system directory", RED_CROSS);
            }
        }
        Err(e) => {
            eprintln!("{} Failed to execute mkdir: {}", RED_CROSS, e);
        }
    }
}

fn read_system_file() -> Result<Config, Box<dyn std::error::Error>> {
    let file = File::open(SYSTEM_FILE)?;
    let reader = BufReader::new(file);
    let config: Config = serde_yaml_ng::from_reader(reader)?;
    Ok( config )
}

fn load_config() -> Config {
    read_system_file().unwrap_or_else(|e| {
        eprintln!("{} Failed to read system file: {}", RED_CROSS, e);
        exit(1);
    })  
}

#[derive(rustyline_derive::Helper, rustyline_derive::Completer, 
         rustyline_derive::Hinter, rustyline_derive::Validator, 
         rustyline_derive::Highlighter)]
struct PathHelper {
    #[rustyline(Completer)]
    completer: FilenameCompleter,
}

fn get_packages_folder() -> String {
    let helper = PathHelper {
        completer: FilenameCompleter::new(),
    };
    
    let mut rl = Editor::new().expect("");  // Use default config
    rl.set_helper(Some(helper));
    
    let input = rl.readline("Enter path for packages folder: ").expect("");
    
    let folder = if let (true, Some(user)) = (input.trim().starts_with("~"), ORIGINAL_USER.get()) {
        format!("/home/{}{}", user, &input.trim()[1..])
    } else {
        input.trim().to_string()
    };
    
    folder
}

fn check_package_installed(package: &str) -> bool {
    Command::new("pacman")
        .args(&["-Qi", package])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn setup_check() {
    ensure_system_directory();

    if Path::new(SYSTEM_FILE).exists() {
        let mut config = load_config();
        let folder = get_packages_folder();
        if Path::new(&folder).is_dir() {
            config.folder = folder;
            if let Err(e) = save_systemfile(&config) {
                eprintln!("{} Error saving systemfile {}", RED_CROSS, e)
            }
        }
    } else {
        println!(
            "{} System file does not exist, starting fresh...",
            YELLOW_WARNING
        );
        let folder = get_packages_folder();
        let mut new_config = Config {
            folder: String::new(),
            packages: Vec::new(),
        };
        if Path::new(&folder).is_dir() {
            new_config.folder = folder;
            if let Err(e) = save_systemfile(&new_config) {
                eprintln!("{} Error saving systemfile {}", RED_CROSS, e)
            }
        }
    }
}

fn update_system() {
    if !check_package_installed("reflector") {
        println!("{} Reflector not installed, installing now", YELLOW_WARNING);
        run_command("pacman -S --noconfirm reflector", true);
    }
    println!("{} Starting update", BLUE_GEAR);
    run_command(
        "reflector --latest 10 --protocol https --sort rate --save /etc/pacman.d/mirrorlist 2>/dev/null",
        true,
    );

    if !check_package_installed("paru") {
        println!("{} Paru not installed, installing now", YELLOW_WARNING);
        run_command("pacman -S --noconfirm paru", true);
    }
    run_command("paru -Syu --noconfirm", false);
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
        let multilib_content = "[multilib]\\nInclude = /etc/pacman.d/mirrorlist\\n";
        let result = Command::new("sudo")
            .arg("sh")
            .arg("-c")
            .arg(format!(
                "echo -e '{}' >> /etc/pacman.conf",
                multilib_content
            ))
            .status();

        match result {
            Ok(status) => {
                if !status.success() {
                    eprintln!("{} Failed to add multilib", RED_CROSS);
                }
            }
            Err(e) => {
                eprintln!("{} Failed to modify pacman.conf: {}", RED_CROSS, e);
            }
        }
    }

    let chaotic_enabled = match OpenOptions::new().read(true).open("/etc/pacman.conf") {
        Ok(file) => BufReader::new(file)
            .lines()
            .map(|line| line.unwrap_or_default())
            .any(|line| line == "[chaotic-aur]"),
        Err(_) => false,
    };

    if !chaotic_enabled {
        println!("Configuring Chaotic-AUR");
        run_command("pacman -Syu", true);
        run_command("pacman-key --init", true);
        run_command("pacman -Sy --noconfirm archlinux-keyring", true);
        run_command(
            "pacman-key --recv-key 3056513887B78AEB --keyserver keyserver.ubuntu.com",
            true,
        );
        run_command("pacman-key --lsign-key 3056513887B78AEB", true);
        run_command(
            "pacman -U --noconfirm 'https://cdn-mirror.chaotic.cx/chaotic-aur/chaotic-keyring.pkg.tar.zst'",
            true,
        );
        run_command(
            "pacman -U --noconfirm 'https://cdn-mirror.chaotic.cx/chaotic-aur/chaotic-mirrorlist.pkg.tar.zst'",
            true,
        );

        let chaotic_content = "[chaotic-aur]\\nInclude = /etc/pacman.d/chaotic-mirrorlist\\n";
        let result = Command::new("sudo")
            .arg("sh")
            .arg("-c")
            .arg(format!("echo -e '{}' >> /etc/pacman.conf", chaotic_content))
            .status();

        match result {
            Ok(status) => {
                if status.success() {
                    println!("{} Chaotic-AUR added", GREEN_CHECK);
                } else {
                    eprintln!("{} Failed to add chaotic-aur to config", RED_CROSS);
                }
            }
            Err(e) => {
                eprintln!("{} Failed to modify pacman.conf: {}", RED_CROSS, e);
            }
        }
        run_command("pacman -Syu --noconfirm", true);
    }
}

fn get_system() -> Result<(Vec<String>, Vec<String>, Vec<String>), Box<dyn std::error::Error>> {
    let alpm = Alpm::new("/", "/var/lib/pacman").expect("Failed to read Database");
    let db = alpm.localdb();
    let packages_installed: Vec<String> =
         db.pkgs().iter().map(|pkg| pkg.name().to_string()).collect();
    let config = load_config();
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
    let mut install_command = format!("paru -S --needed --noconfirm -- ");

    match get_system() {
        Ok((_packages_installed, selected, existing)) => {
            packages_selected = selected;
            existing_packages = existing;
        }
        Err(e) => {
            eprintln!("{} Error reading packages :: {}", RED_CROSS, e);
            return;
        }
    }

    let tobe_installed: Vec<String> = packages_selected
        .into_iter()
        .filter(|item| !existing_packages.contains(item))
        .collect();

    if !tobe_installed.is_empty() {
        install_command.push_str(&tobe_installed.join(" "));
        println!("Packages to install :\n{:?}", (&tobe_installed));
        let confirmation = ask_confirmation("Do you want to proceed installing above packages [Y/n] : ");
        if confirmation {
            let install_status = run_command(&install_command, false);
            if install_status {
                println!("{} All packages installed", GREEN_CHECK);
                let mut config = load_config();
                existing_packages.extend(tobe_installed);
                config.packages = existing_packages;
                match save_systemfile(&config) {
                    Ok(_) => {}
                    Err(_) => eprintln!("Failed to save systemfile"),
                }
            }
        }
    } else {
        println!("{} No package to install", GREEN_CHECK);
    }
}


fn remove_packages() {
    let packages_selected;
    let packages_installed;
    let mut existing_packages;
    let mut remove_command = format!("pacman -Rns --noconfirm ");

    match get_system() {
        Ok((installed, selected, existing)) => {
            packages_selected = selected;
            existing_packages = existing;
            packages_installed = installed;
        }
        Err(e) => {
            eprintln!("Error reading packages :: {}", e);
            return;
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
        println!("Packages to remove :\n{:?}", (&tobe_removed));
        let confirmation = ask_confirmation("Do you want to proceed removing above packages [Y/n] : ");
        if confirmation {
            let removal_status = run_command(&remove_command, true);
            if removal_status {
                existing_packages.retain(|item| !tobe_removed.contains(item));
                let mut config = load_config();
                config.packages = existing_packages;
                match save_systemfile(&config) {
                    Ok(_) => {}
                    Err(_) => eprintln!("Failed to save systemfile"),
                }
            }
        }

    } else {
        println!("{} No package to remove", GREEN_CHECK);
    }
}

fn add_package(packages: &[String]) {
    if packages.is_empty() {
        println!("{} No packages provided", YELLOW_WARNING);
        return;
    }

    let install_command = format!("paru -S --needed -- {}", packages.join(" "));

    run_command(&install_command, false);

    println!("{} Packages installed successfully", GREEN_CHECK);

    let mut config = load_config();
    let manual_install_path = PathBuf::from(&config.folder).join("manual-install.yaml");
    let mut manual_packages: Vec<String> = if manual_install_path.exists() {
        match File::open(&manual_install_path) {
            Ok(file) => {
                let reader = BufReader::new(file);
                serde_yaml_ng::from_reader(reader).unwrap_or_else(|_| Vec::new())
            }
            Err(_) => Vec::new(),
        }
    } else {
        Vec::new()
    };

    for package in packages {
        if !config.packages.contains(package) {
            config.packages.push(package.clone());
            manual_packages.push(package.clone());
        }
    }

    if let Err(e) = save_systemfile(&config) {
        eprintln!("{} Failed to save systemfile: {}", RED_CROSS, e);
        return;
    }

    match OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&manual_install_path)
    {
        Ok(file) => {
            let writer = BufWriter::new(file);
            if let Err(e) = serde_yaml_ng::to_writer(writer, &manual_packages) {
                eprintln!("{} Failed to write manual packages: {}", RED_CROSS, e);
                return;
            }
        }
        Err(e) => {
            eprintln!(
                "{} Failed to create/open manual-install.yaml: {}",
                RED_CROSS, e
            );
            return;
        }
    }
}

fn update_package_files(packages_folder: &str, packages: &[String]) -> Result<(), String> {
    let entries = fs::read_dir(packages_folder)
        .map_err(|e| format!("{} Failed to read packages folder: {}", RED_CROSS, e))?;

    for entry in entries.flatten() {
        let path = entry.path();
        
        if path.is_file() && path.extension().map_or(false, |ext| ext == "yaml") {
            process_yaml_file(&path, packages)?;
        }
    }
    
    Ok(())
}

fn process_yaml_file(path: &Path, packages: &[String]) -> Result<(), String> {
    let filename = path.file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| format!("Invalid filename"))?;

    let file = File::open(path)
        .map_err(|e| format!("{} Failed to open {}: {}", YELLOW_WARNING, filename, e))?;
    
    let mut file_packages: Vec<String> = serde_yaml_ng::from_reader(BufReader::new(file))
        .map_err(|_| format!("{} Failed to parse {}", YELLOW_WARNING, filename))?;

    let original_len = file_packages.len();
    file_packages.retain(|pkg| !packages.contains(pkg));

    if file_packages.len() == original_len {
        return Ok(()); // No changes needed
    }

    if file_packages.is_empty() {
        fs::remove_file(path)
            .map_err(|e| format!("{} Failed to delete {}: {}", RED_CROSS, filename, e))?;
        println!("{} Deleted empty file: {}", GREEN_CHECK, filename);
    } else {
        let file = OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(path)
            .map_err(|e| format!("{} Failed to open {} for writing: {}", RED_CROSS, filename, e))?;
        
        serde_yaml_ng::to_writer(BufWriter::new(file), &file_packages)
            .map_err(|e| format!("{} Failed to write {}: {}", RED_CROSS, filename, e))?;
        
        println!("{} Updated {}", GREEN_CHECK, filename);
    }

    Ok(())
}

fn uninstall_package(packages: &[String]) {
    if packages.is_empty() {
        println!("{} No packages provided", YELLOW_WARNING);
        return;
    }

    let remove_command = format!("pacman -Rns {}", packages.join(" "));

    run_command(&remove_command,true);

    let mut config = load_config();
    config.packages.retain(|pkg| !packages.contains(pkg));

    if let Err(e) = save_systemfile(&config) {
        eprintln!("{} Failed to save systemfile: {}", RED_CROSS, e);
        return;
    }

    update_package_files(&config.folder, packages).expect("Failed to update reomved packages");

    println!("{} Package removal complete", GREEN_CHECK);
}

fn manage_package() {
    if !check_package_installed("paru") {
        println!("{} Paru not installed, installing now", YELLOW_WARNING);
        run_command("pacman -S --noconfirm paru", true);
    }
    install_packages();
    remove_packages();
}


fn initialize() {
    if !check_package_installed("rustup") {
        run_command("pacman -S --noconfirm rustup", true);
        run_command("rustup default stable", false);
    }
    
    setup_check();
    chaotic_aur_setup();
    update_system();
    manage_package();
}

fn update() {
    update_system();
    manage_package();

    let alpm = Alpm::new("/", "/var/lib/pacman").expect("Failed to read database");
    let db = alpm.localdb();
    
    let orphans: Vec<String> = db
        .pkgs()
        .iter()
        .filter(|pkg| {
            pkg.reason() == PackageReason::Depend 
                && pkg.required_by().is_empty()  // Check hard dependencies
                && pkg.optional_for().is_empty()  // Check optional dependencies
        })
        .map(|pkg| pkg.name().to_string())
        .collect();
    
    if !orphans.is_empty() {
        let orphan_list = orphans.join(" ");
        run_command(&format!("pacman -Rns {}", orphan_list), true);
    }
}

fn info() {
    let config = load_config();
    let no_of_packages = config.packages.len();
    let folder = config.folder;
    println!("\nPackages folder : {}", folder);
    println!("No of packages installed : {}", no_of_packages);
}

#[derive(Parser)]
#[clap(
    name = "Declarative Arch",
    about = "Tool to manage archlinux packages declaratively"
)]
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
    #[command(name = "add")]
    Add {
        packages: Vec<String>,
    },
    #[command(name = "remove")]
    Remove {
        packages: Vec<String>,
    },
}

fn main() {
    match get_original_user() {
        Ok(_user) => {}
        Err(e) => {
            eprintln!("{} Error: {}", RED_CROSS, e);
            exit(1);
        }
    }

    let cli = Cli::parse();
    match &cli.command {
        Commands::Init => {
            println!("{} Initializing...", BLUE_GEAR);
            initialize();
        }
        Commands::Install => {
            println!("{} Installing...", BLUE_GEAR);
            manage_package();
        }
        Commands::Update => {
            println!("{} Updating...", BLUE_GEAR);
            update();
        }
        Commands::Info => {
            println!("{} Showing info...", BLUE_GEAR);
            info();
        }
        Commands::Add { packages } => {
            println!("{} Adding packages...", BLUE_GEAR);
            add_package(packages);
        }
        Commands::Remove { packages } => {
            println!("{} Removing packages...", BLUE_GEAR);
            uninstall_package(packages);
        }
    }
}
