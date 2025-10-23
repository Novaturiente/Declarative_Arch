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

fn run_command(command: &str, needs_sudo: bool) {
    let final_command = if needs_sudo {
        format!("sudo {}", command)
    } else {
        command.to_string()
    };

    let result = Command::new("sh")
        .arg("-c")
        .arg(&final_command)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status();

    match result {
        Ok(status) => {
            if !status.success() {
                eprintln!(
                    "{} Command failed with exit code: {:?}",
                    RED_CROSS,
                    status.code()
                );
            }
        }
        Err(e) => {
            eprintln!("{} Failed to execute command: {}", RED_CROSS, e);
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

fn setup_check() {
    ensure_system_directory();

    if Path::new(SYSTEM_FILE).exists() {
        let file = File::open(SYSTEM_FILE).expect("Failed to read systemfile");
        let reader = BufReader::new(file);
        let mut config: Config =
            serde_yaml_ng::from_reader(reader).expect("Failed to read systemfile");

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
                eprintln!("{} Could not determine user", YELLOW_WARNING);
                trimed_input.to_string()
            }
            (false, _) => trimed_input.to_string(),
        };
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
                eprintln!("{} Could not determine user", YELLOW_WARNING);
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
                eprintln!("{} Error saving systemfile {}", RED_CROSS, e)
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
        println!("{} Reflector not installed, installing now", YELLOW_WARNING);
        run_command("pacman -S --noconfirm reflector", true);
    }
    run_command(
        "reflector --latest 10 --protocol https --sort rate --save /etc/pacman.d/mirrorlist 2>/dev/null",
        true,
    );

    let output = Command::new("pacman")
        .args(&["-Qi", "paru"])
        .output()
        .expect("Failed to check for paru");

    if !output.status.success() {
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
                    .stderr(Stdio::piped())  // Capture stderr instead of inheriting
                    .output();  // Use output() instead of status()

                match result {
                    Ok(output) => {
                        // Convert stderr to string for analysis
                        let stderr_output = String::from_utf8_lossy(&output.stderr);
                        
                        if output.status.success() {
                            println!("{} All packages installed", GREEN_CHECK);
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
                            // Print the stderr to console
                            eprint!("{}", stderr_output);
                            
                            // Check if error is network/download related
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
                                }
                            } else {
                                // Non-network error, exit immediately
                                eprintln!(
                                    "\n{} Installation failed. Exiting.",
                                    RED_CROSS
                                );
                                eprintln!("Exit code: {:?}", output.status.code());
                                std::process::exit(1);
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!(
                            "{} Failed to execute command: {} (attempt {})",
                            YELLOW_WARNING, e, attempts
                        );
                        // Command execution errors are usually system issues, exit
                        eprintln!("{} Command execution failed. Exiting.", RED_CROSS);
                        std::process::exit(1);
                    }
                }
            }
        }
    } else {
        println!("{} No package to install", GREEN_CHECK);
    }
}

// Helper function to detect network/download related errors
fn is_network_or_download_error(error_output: &str) -> bool {
    let error_lower = error_output.to_lowercase();
    
    // Common network and download error patterns in pacman/paru
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
        println!("Do you want to proceed removing above packages [Y/n] : ");
        let mut confirmation = String::new();
        io::stdin()
            .read_line(&mut confirmation)
            .expect("Failed to read input");
        let conf = confirmation.trim().to_lowercase();
        if conf == "y" || conf == " " || conf == "" {
            let result = Command::new("sudo")
                .arg("sh")
                .arg("-c")
                .arg(&remove_command)
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .status();

            match result {
                Ok(status) => {
                    if !status.success() {
                        eprintln!("{} Package removal had issues", YELLOW_WARNING);
                    }
                }
                Err(e) => {
                    eprintln!("{} Failed to execute removal: {}", RED_CROSS, e);
                }
            }
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
        println!("{} No package to remove", GREEN_CHECK);
    }
}

fn manage_package() {
    let output = Command::new("pacman")
        .args(&["-Qi", "paru"])
        .output()
        .expect("Failed to check for paru");

    if !output.status.success() {
        println!("{} Paru not installed, installing now", YELLOW_WARNING);
        run_command("pacman -S --noconfirm paru", true);
    }
    install_packages();
    remove_packages();
}

fn add_package(packages: &[String]) {
    if packages.is_empty() {
        println!("{} No packages provided", YELLOW_WARNING);
        return;
    }

    let install_command = format!("paru -S --needed -- {}", packages.join(" "));

    let result = Command::new("sh")
        .arg("-c")
        .arg(&install_command)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status();

    match result {
        Ok(status) => {
            if !status.success() {
                eprintln!("{} Installation failed: {:?}", RED_CROSS, status.code());
                return;
            }
        }
        Err(e) => {
            eprintln!("{} failed to execute paru: {}", RED_CROSS, e);
            return;
        }
    };

    println!("{} Packages installed successfully", GREEN_CHECK);

    let file = match File::open(SYSTEM_FILE) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("{} Failed to open systemfile: {}", RED_CROSS, e);
            return;
        }
    };

    let reader = BufReader::new(file);
    let mut config: Config = match serde_yaml_ng::from_reader(reader) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{} Failed to parse systemfile: {}", RED_CROSS, e);
            return;
        }
    };

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

fn uninstall_package(packages: &[String]) {
    if packages.is_empty() {
        println!("{} No packages provided", YELLOW_WARNING);
        return;
    }

    let remove_command = format!("pacman -Rns {}", packages.join(" "));

    let result = Command::new("sudo")
        .arg("sh")
        .arg("-c")
        .arg(&remove_command)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status();

    match result {
        Ok(status) => {
            if !status.success() {
                eprintln!(
                    "{} Uninstallation failed with exit code: {:?}",
                    YELLOW_WARNING,
                    status.code()
                );
            }
        }
        Err(e) => {
            eprintln!("{} Failed to execute pacman: {}", RED_CROSS, e);
            return;
        }
    }

    let file = match File::open(SYSTEM_FILE) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("{} Failed to open systemfile: {}", RED_CROSS, e);
            return;
        }
    };

    let reader = BufReader::new(file);
    let mut config: Config = match serde_yaml_ng::from_reader(reader) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{} Failed to parse systemfile: {}", RED_CROSS, e);
            return;
        }
    };

    config.packages.retain(|pkg| !packages.contains(pkg));

    if let Err(e) = save_systemfile(&config) {
        eprintln!("{} Failed to save systemfile: {}", RED_CROSS, e);
        return;
    }

    let packages_folder = &config.folder;

    match fs::read_dir(packages_folder) {
        Ok(entries) => {
            for entry in entries {
                if let Ok(entry) = entry {
                    let path = entry.path();

                    if path.is_file() && path.extension().map_or(false, |ext| ext == "yaml") {
                        if let Some(filename) = path.file_name().and_then(|n| n.to_str()) {
                            match File::open(&path) {
                                Ok(file) => {
                                    let reader = BufReader::new(file);
                                    let mut file_packages: Vec<String> =
                                        match serde_yaml_ng::from_reader(reader) {
                                            Ok(pkgs) => pkgs,
                                            Err(_) => {
                                                eprintln!(
                                                    "{} Failed to parse {}",
                                                    YELLOW_WARNING, filename
                                                );
                                                continue;
                                            }
                                        };

                                    let original_len = file_packages.len();
                                    file_packages.retain(|pkg| !packages.contains(pkg));

                                    if file_packages.len() != original_len {
                                        if file_packages.is_empty() {
                                            match fs::remove_file(&path) {
                                                Ok(_) => {
                                                    println!(
                                                        "{} Deleted empty file: {}",
                                                        GREEN_CHECK, filename
                                                    );
                                                }
                                                Err(e) => {
                                                    eprintln!(
                                                        "{} Failed to delete {}: {}",
                                                        RED_CROSS, filename, e
                                                    );
                                                }
                                            }
                                        } else {
                                            match OpenOptions::new()
                                                .write(true)
                                                .truncate(true)
                                                .open(&path)
                                            {
                                                Ok(file) => {
                                                    let writer = BufWriter::new(file);
                                                    if let Err(e) = serde_yaml_ng::to_writer(
                                                        writer,
                                                        &file_packages,
                                                    ) {
                                                        eprintln!(
                                                            "{} Failed to write {}: {}",
                                                            RED_CROSS, filename, e
                                                        );
                                                    } else {
                                                        println!(
                                                            "{} Updated {}",
                                                            GREEN_CHECK, filename
                                                        );
                                                    }
                                                }
                                                Err(e) => {
                                                    eprintln!(
                                                        "{} Failed to open {} for writing: {}",
                                                        RED_CROSS, filename, e
                                                    );
                                                }
                                            }
                                        }
                                    }
                                }
                                Err(e) => {
                                    eprintln!(
                                        "{} Failed to open {}: {}",
                                        YELLOW_WARNING, filename, e
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
        Err(e) => {
            eprintln!("{} Failed to read packages folder: {}", RED_CROSS, e);
        }
    }

    println!("{} Package removal complete", GREEN_CHECK);
}

fn initialize() {
    run_command("pacman -S --noconfirm rustup", true);
    run_command("rustup default stable", false);
    
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
        // Use your collected orphans instead of re-querying
        let orphan_list = orphans.join(" ");
        run_command(&format!("pacman -Rns {}", orphan_list), true);
    }
}

fn info() {
    let file = File::open(SYSTEM_FILE).expect("Failed to read systemfile");
    let reader = BufReader::new(file);
    let config: Config = serde_yaml_ng::from_reader(reader).expect("Failed to read systemfile");
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
        /// Package names to add
        packages: Vec<String>,
    },
    #[command(name = "remove")]
    Remove {
        /// Package names to remove
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
