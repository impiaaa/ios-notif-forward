use iconwriter::{ico, Icon, Image};
use std::{env, fs, io};
use winres::WindowsResource;

fn main() -> io::Result<()> {
    if env::var_os("CARGO_CFG_WINDOWS").is_some() {
        let mut icon = ico::Ico::new();
        icon.add_entries(
            iconwriter::resample::linear,
            &Image::open(format!(
                "{}/icon.svg",
                env::var_os("CARGO_MANIFEST_DIR")
                    .unwrap()
                    .into_string()
                    .unwrap()
            ))?,
            vec![ico::Key(0), ico::Key(32), ico::Key(64), ico::Key(128)],
        )
        .unwrap();
        let iconpath = format!(
            "{}/icon.ico",
            env::var_os("OUT_DIR").unwrap().into_string().unwrap()
        );
        icon.write(&mut fs::File::create(&iconpath)?)?;

        WindowsResource::new()
            // This path can be absolute, or relative to your crate root.
            .set_icon(&iconpath)
            .compile()?;
    }
    Ok(())
}
