//! Builds Examail.app, and for a release signs it with the Developer ID certificate, notarizes it
//! and wraps it in a disk image.
//!
//! - `cargo xtask build` makes `target/release/Examail.app`, signed ad hoc, or as
//!   `EXO_SIGN_IDENTITY` when that's set.
//! - `cargo xtask release` is what `.github/workflows/release.yml` runs for a tag. It needs the
//!   Google client (`EXO_GOOGLE_CLIENT_ID`, `EXO_GOOGLE_CLIENT_SECRET`), the certificate
//!   (`DEVELOPER_ID_P12_BASE64`, `DEVELOPER_ID_P12_PASSWORD`) and an App Store Connect API key
//!   (`APP_STORE_CONNECT_API_KEY_KEY_ID`, `APP_STORE_CONNECT_API_KEY_KEY`,
//!   `APP_STORE_CONNECT_API_KEY_ISSUER_ID`), and leaves the disk image and `checksums.txt` in
//!   `target/release`.
//!
//! Everything here drives Apple's own tools (`actool`, `codesign`, `security`, `notarytool`,
//! `stapler`, `hdiutil`), so building needs only Rust and Xcode.

use std::ffi::OsStr;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::{env, fs};

use anyhow::{Context, Result, bail, ensure};
use base64::Engine as _;

const NAME: &str = "Examail";
const MINIMUM_MACOS: &str = "13.0";
const BUNDLE_ID: &str = "com.andrewbenson.Examail";
/// The workspace's version, which every crate inherits, `exo` included.
const VERSION: &str = env!("CARGO_PKG_VERSION");
/// The certificate codesign signs a release with, found in the release keychain.
const DEVELOPER_ID: &str = "Developer ID Application";

fn main() -> Result<()> {
    match env::args().nth(1).as_deref() {
        Some("build") => build(),
        Some("release") => release(),
        _ => {
            eprintln!("usage: cargo xtask build | release");
            std::process::exit(64);
        }
    }
}

/// `cargo xtask build`: the app, signed ad hoc or as `EXO_SIGN_IDENTITY`.
fn build() -> Result<()> {
    let identity = env::var("EXO_SIGN_IDENTITY").unwrap_or_default();
    let app = bundle()?;
    sign(&app, if identity.is_empty() { "-" } else { &identity }, false, false)?;
    let signed = if identity.is_empty() {
        "ad hoc".to_owned()
    } else {
        format!("as {identity}")
    };
    println!("{} ({VERSION}, signed {signed})", app.display());
    Ok(())
}

/// `cargo xtask release`: the app and its disk image, signed with the Developer ID certificate
/// and notarized, plus the image's checksum.
fn release() -> Result<()> {
    ensure!(
        !var("EXO_GOOGLE_CLIENT_ID").is_empty() && !var("EXO_GOOGLE_CLIENT_SECRET").is_empty(),
        "Set EXO_GOOGLE_CLIENT_ID and EXO_GOOGLE_CLIENT_SECRET, so the release has a built-in sign-in client."
    );
    let tag = var("RELEASE_TAG");
    ensure!(
        tag.is_empty() || tag == format!("v{VERSION}"),
        "The release is tagged {tag}, but Examail is {VERSION}."
    );
    let work = Scratch::new()?;
    let app = bundle()?;

    let keychain = Keychain::create(&work.0)?;
    keychain.import_certificate(&work.0)?;
    sign(&app, DEVELOPER_ID, true, true)?;

    let key = ApiKey::from_env(&work.0)?;
    // notarytool takes an archive of the app, and the ticket is stapled to the app itself.
    let archive = work.0.join(format!("{NAME}.zip"));
    run("ditto", [os("-c"), os("-k"), os("--keepParent"), app.as_os_str(), archive.as_os_str()])?;
    key.notarize(&archive)?;
    staple(&app)?;
    run(
        "spctl",
        [os("--assess"), os("--type"), os("execute"), os("--verbose=4"), app.as_os_str()],
    )?;

    let image = disk_image(&app)?;
    sign(&image, DEVELOPER_ID, false, true)?;
    key.notarize(&image)?;
    staple(&image)?;
    run(
        "spctl",
        [
            os("--assess"),
            os("--type"),
            os("open"),
            os("--context"),
            os("context:primary-signature"),
            os("--verbose=4"),
            image.as_os_str(),
        ],
    )?;

    let directory = image.parent().context("the image has a folder")?;
    let name = image.file_name().context("the image has a name")?;
    let sums = output(Command::new("shasum").args([os("-a"), os("256"), name]).current_dir(directory))?;
    fs::write(directory.join("checksums.txt"), sums)?;
    println!("{} ({VERSION}, notarized)", image.display());
    Ok(())
}

/// Builds `exo` and bundles it as `target/release/Examail.app`, unsigned. The bundle is put
/// together beside it and moved into place last, so a failed build never leaves half an app.
fn bundle() -> Result<PathBuf> {
    let root = root();
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    run_in(&root, cargo, ["build", "--release", "--bin", "exo"])?;
    let release = target_dir(&root).join("release");

    let app = release.join(format!("{NAME}.app"));
    let partial = release.join(format!("{NAME}.app.partial"));
    remove(&partial)?;
    let contents = partial.join("Contents");
    let resources = contents.join("Resources");
    fs::create_dir_all(contents.join("MacOS"))?;
    fs::create_dir_all(&resources)?;
    fs::copy(release.join("exo"), contents.join("MacOS").join(NAME))?;

    copy_tree(&root.join("assets/images"), &resources.join("assets/images"))?;

    let icon_plist = release.join("AppIcon.plist");
    run_in(
        &root,
        "xcrun",
        [
            os("actool"),
            os("assets/AppIcon.icon"),
            os("--compile"),
            resources.as_os_str(),
            os("--platform"),
            os("macosx"),
            os("--minimum-deployment-target"),
            os(MINIMUM_MACOS),
            os("--app-icon"),
            os("AppIcon"),
            os("--output-partial-info-plist"),
            icon_plist.as_os_str(),
            os("--output-format"),
            os("human-readable-text"),
        ],
    )?;
    remove(&icon_plist)?;

    fs::copy(root.join("THIRD_PARTY_NOTICES.md"), resources.join("THIRD_PARTY_NOTICES.md"))?;
    copy_tree(&root.join("assets/inbox-zero"), &resources.join("inbox-zero"))?;
    for (folder, notices) in [("fonts", ["OFL.txt", "SOURCES.txt"]), ("icons", ["LICENSE", "SOURCES.txt"])] {
        fs::create_dir_all(resources.join(folder))?;
        for notice in notices {
            fs::copy(
                root.join("assets").join(folder).join(notice),
                resources.join(folder).join(notice),
            )?;
        }
    }
    fs::write(contents.join("Info.plist"), info_plist())?;

    run("xattr", [os("-cr"), partial.as_os_str()])?;
    remove(&app)?;
    fs::rename(&partial, &app)?;
    Ok(app)
}

/// A value in `Info.plist`.
enum Value {
    String(&'static str),
    Bool(bool),
    Array(Vec<Value>),
    Dict(Vec<(&'static str, Value)>),
}

/// The app's `Info.plist`, as XML, its keys sorted.
fn info_plist() -> String {
    use Value::{Array, Bool, Dict, String as S};
    let url_type = |role, name, schemes: &[&'static str]| {
        Dict(vec![
            ("CFBundleTypeRole", S(role)),
            ("CFBundleURLName", S(name)),
            ("CFBundleURLSchemes", Array(schemes.iter().map(|scheme| S(scheme)).collect())),
        ])
    };
    let version: &'static str = VERSION;
    let mut keys = vec![
        ("CFBundleName", S(NAME)),
        ("CFBundleExecutable", S(NAME)),
        ("CFBundleVersion", S(version)),
        ("CFBundleDisplayName", S(NAME)),
        ("CFBundleIconFile", S("AppIcon")),
        ("CFBundleIconName", S("AppIcon")),
        ("CFBundlePackageType", S("APPL")),
        ("CFBundleIdentifier", S(BUNDLE_ID)),
        ("NSHighResolutionCapable", Bool(true)),
        ("NSQuitAlwaysKeepsWindows", Bool(false)),
        ("NSPrincipalClass", S("NSApplication")),
        ("CFBundleShortVersionString", S(version)),
        ("CFBundleInfoDictionaryVersion", S("6.0")),
        ("LSMinimumSystemVersion", S(MINIMUM_MACOS)),
        ("NSRequiresAquaSystemAppearance", Bool(false)),
        ("NSSupportsAutomaticGraphicsSwitching", Bool(true)),
        ("NSPrefersDisplaySafeAreaCompatibilityMode", Bool(false)),
        ("LSApplicationCategoryType", S("public.app-category.productivity")),
        (
            "NSDownloadsFolderUsageDescription",
            S("Examail saves the attachments you download to your Downloads folder."),
        ),
        (
            "CFBundleURLTypes",
            Array(vec![
                url_type("Viewer", "Examail", &["examail", "exo"]),
                url_type("Editor", "Email", &["mailto"]),
            ]),
        ),
    ];
    keys.sort_by_key(|(key, _)| *key);

    let mut xml = String::from(concat!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
        "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n",
        "<plist version=\"1.0\">\n",
    ));
    write_value(&mut xml, &Dict(keys), 0);
    xml.push_str("</plist>\n");
    xml
}

fn write_value(xml: &mut String, value: &Value, depth: usize) {
    let indent = "\t".repeat(depth);
    match value {
        Value::String(text) => {
            let _ = writeln!(xml, "{indent}<string>{}</string>", escape(text));
        }
        Value::Bool(true) => {
            let _ = writeln!(xml, "{indent}<true/>");
        }
        Value::Bool(false) => {
            let _ = writeln!(xml, "{indent}<false/>");
        }
        Value::Array(items) => {
            let _ = writeln!(xml, "{indent}<array>");
            for item in items {
                write_value(xml, item, depth + 1);
            }
            let _ = writeln!(xml, "{indent}</array>");
        }
        Value::Dict(entries) => {
            let _ = writeln!(xml, "{indent}<dict>");
            for (key, item) in entries {
                let _ = writeln!(xml, "{indent}\t<key>{}</key>", escape(key));
                write_value(xml, item, depth + 1);
            }
            let _ = writeln!(xml, "{indent}</dict>");
        }
    }
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Wraps `app` in `examail-vX.Y.Z-macos-arm64.dmg` beside it, with a link to Applications to drag
/// it onto.
fn disk_image(app: &Path) -> Result<PathBuf> {
    let directory = app.parent().context("the app has a folder")?;
    let image = directory.join(format!("examail-v{VERSION}-macos-arm64.dmg"));
    let contents = directory.join(format!("examail-v{VERSION}-macos-arm64.dmg.partial"));
    remove(&contents)?;
    remove(&image)?;
    fs::create_dir_all(&contents)?;
    let name = app.file_name().context("the app has a name")?;
    run("ditto", [app.as_os_str(), contents.join(name).as_os_str()])?;
    std::os::unix::fs::symlink("/Applications", contents.join("Applications"))?;
    run(
        "hdiutil",
        [
            os("create"),
            os("-volname"),
            os(NAME),
            os("-srcfolder"),
            contents.as_os_str(),
            os("-format"),
            os("UDZO"),
            image.as_os_str(),
        ],
    )?;
    remove(&contents)?;
    run("hdiutil", [os("verify"), image.as_os_str()])?;
    Ok(image)
}

/// Signs `path` as `identity` (`-` for ad hoc), with the hardened runtime and a secure timestamp
/// when asked, then checks the signature.
fn sign(path: &Path, identity: &str, hardened: bool, timestamped: bool) -> Result<()> {
    let mut args = vec![os("--force"), os("--sign"), os(identity)];
    if hardened {
        args.extend([os("--options"), os("runtime")]);
    }
    if timestamped {
        args.push(os("--timestamp"));
    }
    args.push(path.as_os_str());
    run("codesign", args)?;
    run("codesign", [os("--verify"), os("--strict"), os("--deep"), path.as_os_str()])
}

/// Staples the notarization ticket to `path` and checks it's there.
fn staple(path: &Path) -> Result<()> {
    run("xcrun", [os("stapler"), os("staple"), path.as_os_str()])?;
    run("xcrun", [os("stapler"), os("validate"), path.as_os_str()])
}

/// A temporary keychain that holds the Developer ID certificate while a release is signed. It
/// becomes the user's default keychain and joins their search list, so codesign finds the
/// certificate. Dropping it puts the search list and default back and deletes it.
struct Keychain {
    path: PathBuf,
    search_list: Vec<String>,
    default: Option<String>,
}

impl Keychain {
    fn create(work: &Path) -> Result<Self> {
        let path = work.join("examail-release.keychain-db");
        let search_list = keychains(output(Command::new("security").args(["list-keychains", "-d", "user"]))?);
        let default = keychains(output(Command::new("security").args(["default-keychain", "-d", "user"]))?)
            .into_iter()
            .next();
        run("security", [os("create-keychain"), os("-p"), os(""), path.as_os_str()])?;
        let keychain = Self {
            path,
            search_list,
            default,
        };
        let path = keychain.path.as_os_str();
        // Locks itself after an hour, longer than a release takes.
        run("security", [os("set-keychain-settings"), os("-lut"), os("3600"), path])?;
        run("security", [os("unlock-keychain"), os("-p"), os(""), path])?;
        let mut list = vec![os("list-keychains"), os("-d"), os("user"), os("-s"), path];
        list.extend(keychain.search_list.iter().map(os));
        run("security", list)?;
        run("security", [os("default-keychain"), os("-d"), os("user"), os("-s"), path])?;
        Ok(keychain)
    }

    /// Imports the certificate and its key from `DEVELOPER_ID_P12_BASE64` (a .p12 exported from
    /// Keychain Access, base64-encoded) and lets codesign use the key without asking.
    fn import_certificate(&self, work: &Path) -> Result<()> {
        let encoded: String = env::var("DEVELOPER_ID_P12_BASE64")
            .context("DEVELOPER_ID_P12_BASE64 isn't set")?
            .split_whitespace()
            .collect();
        let password = env::var("DEVELOPER_ID_P12_PASSWORD").context("DEVELOPER_ID_P12_PASSWORD isn't set")?;
        let p12 = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .context("DEVELOPER_ID_P12_BASE64 isn't base64")?;
        let file = work.join("developer-id.p12");
        fs::write(&file, p12)?;
        let imported = run(
            "security",
            [
                os("import"),
                file.as_os_str(),
                os("-k"),
                self.path.as_os_str(),
                os("-P"),
                os(&password),
                os("-T"),
                os("/usr/bin/codesign"),
                os("-T"),
                os("/usr/bin/security"),
            ],
        );
        let _ = fs::remove_file(&file);
        imported?;
        quiet(
            "security",
            [
                os("set-key-partition-list"),
                os("-S"),
                os("apple-tool:,apple:,codesign:"),
                os("-s"),
                os("-k"),
                os(""),
                self.path.as_os_str(),
            ],
        )
    }
}

impl Drop for Keychain {
    fn drop(&mut self) {
        let mut list = vec![os("list-keychains"), os("-d"), os("user"), os("-s")];
        list.extend(self.search_list.iter().map(os));
        let _ = run("security", list);
        if let Some(default) = &self.default {
            let _ = run("security", [os("default-keychain"), os("-d"), os("user"), os("-s"), os(default)]);
        }
        let _ = run("security", [os("delete-keychain"), self.path.as_os_str()]);
    }
}

/// The keychain paths `security` lists, one quoted path a line.
fn keychains(listing: String) -> Vec<String> {
    listing
        .lines()
        .map(|line| line.trim().trim_matches('"').to_owned())
        .filter(|path| !path.is_empty())
        .collect()
}

/// The App Store Connect API key notarytool signs in with, written to a .p8 file for it.
struct ApiKey {
    id: String,
    issuer: String,
    file: PathBuf,
}

impl ApiKey {
    fn from_env(work: &Path) -> Result<Self> {
        let id = env::var("APP_STORE_CONNECT_API_KEY_KEY_ID").context("APP_STORE_CONNECT_API_KEY_KEY_ID isn't set")?;
        let issuer =
            env::var("APP_STORE_CONNECT_API_KEY_ISSUER_ID").context("APP_STORE_CONNECT_API_KEY_ISSUER_ID isn't set")?;
        let content = env::var("APP_STORE_CONNECT_API_KEY_KEY").context("APP_STORE_CONNECT_API_KEY_KEY isn't set")?;
        let file = work.join(format!("AuthKey_{id}.p8"));
        fs::write(&file, pem(&content)?)?;
        Ok(Self { id, issuer, file })
    }

    /// Sends `path` to Apple's notary service and waits for its verdict. The service's log is
    /// printed either way, as it says why a submission was turned down.
    fn notarize(&self, path: &Path) -> Result<()> {
        println!("Notarizing {}", path.display());
        let key = [
            os("--key"),
            self.file.as_os_str(),
            os("--key-id"),
            os(&self.id),
            os("--issuer"),
            os(&self.issuer),
        ];
        let submitted = Command::new("xcrun")
            .args([os("notarytool"), os("submit"), path.as_os_str(), os("--wait"), os("--output-format"), os("json")])
            .args(key)
            .stderr(Stdio::inherit())
            .output()
            .context("running notarytool")?;
        let answer: serde_json::Value = serde_json::from_slice(&submitted.stdout)
            .with_context(|| format!("notarytool's answer: {}", String::from_utf8_lossy(&submitted.stdout)))?;
        let status = answer["status"].as_str().unwrap_or_default();
        if let Some(id) = answer["id"].as_str() {
            let _ = Command::new("xcrun").args([os("notarytool"), os("log"), os(id)]).args(key).status();
        }
        ensure!(status == "Accepted", "notarizing {} ended {status:?}: {answer}", path.display());
        Ok(())
    }
}

/// The key as a PEM file. The secret may hold it as written, with `\n` for its line breaks, on a
/// single line, or base64-encoded.
fn pem(content: &str) -> Result<String> {
    let mut text = content.trim().replace("\\n", "\n");
    if !text.starts_with("-----BEGIN") {
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(text.split_whitespace().collect::<String>())
            .context("APP_STORE_CONNECT_API_KEY_KEY is neither a PEM key nor base64")?;
        text = String::from_utf8(decoded).context("the decoded API key isn't text")?.trim().to_owned();
    }
    const BEGIN: &str = "-----BEGIN PRIVATE KEY-----";
    const END: &str = "-----END PRIVATE KEY-----";
    let body: String = text
        .strip_prefix(BEGIN)
        .and_then(|rest| rest.trim_end().strip_suffix(END))
        .context("APP_STORE_CONNECT_API_KEY_KEY isn't a PEM private key")?
        .split_whitespace()
        .collect();
    let mut pem = format!("{BEGIN}\n");
    for line in body.as_bytes().chunks(64) {
        pem.push_str(std::str::from_utf8(line)?);
        pem.push('\n');
    }
    pem.push_str(END);
    pem.push('\n');
    Ok(pem)
}

/// A private folder for a release's passing files (the certificate, the API key, the keychain),
/// deleted when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Result<Self> {
        let base = env::var_os("RUNNER_TEMP").map_or_else(env::temp_dir, PathBuf::from);
        let path = base.join(format!("examail-release-{}", std::process::id()));
        remove(&path)?;
        fs::create_dir_all(&path)?;
        fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
        Ok(Self(path))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Copies `source` to `destination` (whose parent is made), keeping modes and times, without the
/// Finder's `.DS_Store` files.
fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir_all(destination.parent().context("the destination has a folder")?)?;
    run("cp", [os("-Rp"), source.as_os_str(), destination.as_os_str()])?;
    remove_ds_store(destination)
}

fn remove_ds_store(directory: &Path) -> Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            remove_ds_store(&entry.path())?;
        } else if entry.file_name() == ".DS_Store" {
            fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

/// Removes a file or folder, if there is one.
fn remove(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(path)?,
        Ok(_) => fs::remove_file(path)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

/// The workspace's root.
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/xtask sits two folders down")
        .to_owned()
}

/// Where cargo builds: `CARGO_TARGET_DIR`, else `target` in the workspace.
fn target_dir(root: &Path) -> PathBuf {
    env::var_os("CARGO_TARGET_DIR").map_or_else(|| root.join("target"), PathBuf::from)
}

/// An environment variable, empty when it isn't set.
fn var(name: &str) -> String {
    env::var(name).unwrap_or_default()
}

fn os<S: AsRef<OsStr> + ?Sized>(text: &S) -> &OsStr {
    text.as_ref()
}

/// Runs a command, failing when it does.
fn run<I, S>(program: impl AsRef<OsStr>, args: I) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    status(Command::new(program).args(args))
}

fn run_in<I, S>(directory: &Path, program: impl AsRef<OsStr>, args: I) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    status(Command::new(program).args(args).current_dir(directory))
}

/// Runs a command whose output says nothing useful, showing it only when it fails.
fn quiet<I, S>(program: impl AsRef<OsStr>, args: I) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = Command::new(program);
    command.args(args);
    let done = command.output().with_context(|| format!("running {}", describe(&command)))?;
    if !done.status.success() {
        bail!(
            "{} failed ({}): {}",
            describe(&command),
            done.status,
            String::from_utf8_lossy(&done.stderr)
        );
    }
    Ok(())
}

fn status(command: &mut Command) -> Result<()> {
    println!("$ {}", describe(command));
    let done = command.status().with_context(|| format!("running {}", describe(command)))?;
    ensure!(done.success(), "{} failed ({done})", describe(command));
    Ok(())
}

/// Runs a command and returns what it printed.
fn output(command: &mut Command) -> Result<String> {
    let done = command
        .stderr(Stdio::inherit())
        .output()
        .with_context(|| format!("running {}", describe(command)))?;
    ensure!(done.status.success(), "{} failed ({})", describe(command), done.status);
    Ok(String::from_utf8(done.stdout)?)
}

/// The command as a shell line, for the log. The certificate's password is left out.
fn describe(command: &Command) -> String {
    let mut line = command.get_program().to_string_lossy().into_owned();
    let mut hide = false;
    for arg in command.get_args() {
        line.push(' ');
        if hide {
            line.push_str("***");
        } else {
            line.push_str(&arg.to_string_lossy());
        }
        hide = arg == "-P" || arg == "-p";
    }
    line
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;

    use super::pem;

    const BODY: &str = "MIGTAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBHkwdwIBAQQgAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    const WRAPPED: &str = "-----BEGIN PRIVATE KEY-----\nMIGTAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBHkwdwIBAQQgAAAAAAAAAAAAAAAA\nAAAAAAAAAAAAAAAAAAAAAAAAAAA\n-----END PRIVATE KEY-----\n";

    #[test]
    fn a_key_as_written_comes_back_the_same() {
        assert_eq!(pem(WRAPPED).unwrap(), WRAPPED);
    }

    #[test]
    fn a_key_on_one_line_is_wrapped() {
        let single = format!("-----BEGIN PRIVATE KEY----- {BODY} -----END PRIVATE KEY-----");
        assert_eq!(pem(&single).unwrap(), WRAPPED);
    }

    #[test]
    fn escaped_line_breaks_are_read_as_breaks() {
        assert_eq!(pem(&WRAPPED.replace('\n', "\\n")).unwrap(), WRAPPED);
    }

    #[test]
    fn a_base64_encoded_key_is_decoded() {
        let encoded = base64::engine::general_purpose::STANDARD.encode(WRAPPED);
        assert_eq!(pem(&encoded).unwrap(), WRAPPED);
    }

    #[test]
    fn something_else_is_refused() {
        assert!(pem("not a key").is_err());
    }
}
