use async_zip::tokio::write::ZipFileWriter;
use async_zip::{AttributeCompatibility, Compression, ZipEntryBuilder};
use clap::{arg, command, value_parser};
use derive_more::From;
use iconwriter::{icns, Icon, IconError, Image};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use tokio::fs;
use tokio::io;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

macro_rules! INFO_PLIST_FMT { () => { "<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">
<plist version=\"1.0\">
<dict>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>NSBluetoothAlwaysUsageDescription</key>
    <string>Connecting to your device and receive its notifications</string>
    <key>CFBundleExecutable</key>
    <string>{}</string>
    <key>CFBundleIconFile</key>
    <string>Icon.icns</string>
    <key>CFBundleIdentifier</key>
    <string>net.boatcake.{}</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleVersion</key>
    <string>{}</string>
    <key>LSUIElement</key>
    <true/>
    <key>LSMinimumSystemVersion</key>
    <string>10.15.0</string>
</dict>
</plist>" }; }

macro_rules! DESKTOP_ENTRY_FMT {
    () => {
        "[Desktop Entry]
Type=Application
Version={}
Name={}
Icon={}
Exec={}
Terminal=false
X-GNOME-UsesNotifications=true"
    };
}

#[derive(Debug, From)]
enum Error {
    Io(io::Error),
    Zip(async_zip::error::ZipError),
    Icns(IconError<icns::Key>),
    Toml(toml::de::Error),
    Triple(target_spec::errors::TripleParseError),
    TargetSpec(target_spec::Error),
}

#[cfg(windows)]
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

trait IoResultExt {
    fn exist_ok(self) -> Self;
}

impl IoResultExt for io::Result<()> {
    fn exist_ok(self) -> io::Result<()> {
        match self {
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(()),
            _ => self,
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let matches = command!()
        .arg(
            clap::Arg::new("manifest-path")
                .long("manifest-path")
                .value_name("FILE")
                .required(false)
                .value_parser(value_parser!(PathBuf)),
        )
        .arg(arg!(--target <TRIPLE>).required(false))
        .get_matches();

    let manifest_path;
    let manifest_dir;
    if let Some(manifest_path_inner1) = matches.get_one::<PathBuf>("manifest-path") {
        manifest_dir = manifest_path_inner1.parent().unwrap();
        manifest_path = manifest_path_inner1.clone();
    } else {
        let my_manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        manifest_dir = my_manifest_dir.parent().unwrap().parent().unwrap();
        manifest_path = manifest_dir.join("Cargo.toml");
    };
    println!("Using manifest at {:?}", &manifest_path);

    let target = matches
        .get_one::<String>("target")
        .map(|v| v.as_str())
        .unwrap_or(env!("TARGET"));
    println!("Using target triple {target}");

    // ensure a valid target triple
    let _ = target_spec::Triple::from_str(target)?;

    // parse out project toml
    let mut manifile = fs::File::open(&manifest_path).await?;
    let mut manidata = String::new();
    manifile.read_to_string(&mut manidata).await?;
    let manifest = manidata.parse::<toml::Table>()?;
    let pkg_info = &manifest["package"];
    let pkg_name = pkg_info["name"].as_str().expect("Package has no name");
    let pkg_version = pkg_info["version"]
        .as_str()
        .expect("Package has no version");

    println!("Building {pkg_name} version {pkg_version}");
    Command::new("cargo")
        .arg("build")
        .arg("--release")
        .arg(format!(
            "--manifest-path={}",
            &manifest_path.to_str().unwrap()
        ))
        .arg(format!("--target={target}"))
        .spawn()?
        .wait()
        .await?;

    let zipname = format!("{pkg_name}-{pkg_version}-{target}.zip");
    println!("Zipping to {zipname}");
    let mut zipfile = fs::File::create(zipname).await?;
    let mut zipwriter = ZipFileWriter::with_tokio(&mut zipfile);

    if target_spec::eval("cfg(target_os = \"macos\")", target)?.unwrap() {
        let mut exefile = fs::File::open(
            manifest_dir
                .join("target")
                .join(target)
                .join("release")
                .join(pkg_name),
        )
        .await?;
        let mut exedata = Vec::new();
        exefile.read_to_end(&mut exedata).await?;

        let exeentry = ZipEntryBuilder::new(
            format!("{pkg_name}.app/Contents/MacOS/{pkg_name}").into(),
            Compression::Deflate,
        )
        .attribute_compatibility(AttributeCompatibility::Unix)
        .unix_permissions(0o755)
        .build();
        zipwriter.write_entry_whole(exeentry, &exedata).await?;

        let mut icon = icns::Icns::new();
        icon.add_entries(
            iconwriter::resample::linear,
            &Image::open(manifest_dir.join("icon.svg"))?,
            vec![
                icns::Key::Rgba16,
                icns::Key::Rgba32,
                icns::Key::Rgba64,
                icns::Key::Rgba128,
                icns::Key::Rgba256,
                icns::Key::Rgba512,
                icns::Key::Rgba1024,
            ],
        )?;
        let mut icondata = Vec::new();
        icon.write(&mut icondata)?;

        let iconentry = ZipEntryBuilder::new(
            format!("{pkg_name}.app/Contents/Resources/Icon.icns").into(),
            Compression::Deflate,
        )
        .build();
        zipwriter.write_entry_whole(iconentry, &icondata).await?;

        let infoentry = ZipEntryBuilder::new(
            format!("{pkg_name}.app/Contents/Info.plist").into(),
            Compression::Deflate,
        )
        .build();
        zipwriter
            .write_entry_whole(
                infoentry,
                format!(INFO_PLIST_FMT!(), pkg_name, pkg_name, pkg_version).as_bytes(),
            )
            .await?;
    } else if target_spec::eval("cfg(windows)", target)?.unwrap() {
        let exename = format!("{pkg_name}.exe");
        let mut exefile = fs::File::open(
            manifest_dir
                .join("target")
                .join(target)
                .join("release")
                .join(&exename),
        )
        .await?;
        let mut exedata = Vec::new();
        exefile.read_to_end(&mut exedata).await?;

        let exeentry = ZipEntryBuilder::new(exename.into(), Compression::Deflate).build();
        zipwriter.write_entry_whole(exeentry, &exedata).await?;
    } else if target_spec::eval("cfg(unix)", target)?.unwrap() {
        let mut exefile = fs::File::open(
            manifest_dir
                .join("target")
                .join(target)
                .join("release")
                .join(pkg_name),
        )
        .await?;
        let mut exedata = Vec::new();
        exefile.read_to_end(&mut exedata).await?;

        let exeentry = ZipEntryBuilder::new(format!("bin/{pkg_name}").into(), Compression::Deflate)
            .attribute_compatibility(AttributeCompatibility::Unix)
            .unix_permissions(0o755)
            .build();
        zipwriter.write_entry_whole(exeentry, &exedata).await?;

        let mut iconfile = fs::File::open(manifest_dir.join("icon.svg")).await?;
        let mut icondata = Vec::new();
        iconfile.read_to_end(&mut icondata).await?;

        let iconentry = ZipEntryBuilder::new(
            format!("share/icons/hicolor/scalable/apps/{pkg_name}.svg").into(),
            Compression::Deflate,
        )
        .build();
        zipwriter.write_entry_whole(iconentry, &icondata).await?;

        let desktopentry = ZipEntryBuilder::new(
            format!("share/applications/{pkg_name}.desktop").into(),
            Compression::Deflate,
        )
        .build();
        zipwriter
            .write_entry_whole(
                desktopentry,
                format!(
                    DESKTOP_ENTRY_FMT!(),
                    pkg_version, pkg_name, pkg_name, pkg_name
                )
                .as_bytes(),
            )
            .await?;
    }

    zipwriter.close().await?;
    Ok(())
}
