/******************************************************************************

COPYRIGHT 2026, Seho Lee.

This file is provided by the copyright holders under the following terms:

- Any person, group, company, government body or other entity that obtains a
  copy of this file is permitted to use, copy, produce, modify and distribute it
  for any lawful purpose, including commercial use.

- Any copy, production, modification and distribution of this file must retain
  the copyright notice above. If this file is used in a physical or digital
  product, the copyright notice must be included in the documents or other
  materials provided with the product, where reasonably practicable.

- If anyone begins any patent action against any entity, including a cross-claim
  or counterclaim lawsuit, alleging that this file or the contribution within it
  infringes their patent, any patent licences and permissions granted to the
  claimant under this licence terminate as of the date that such patent action
  is filed.

- This file is provided ‘As Is’ without any explicit or implied warranty. Any
  warranty, including the warranties of merchantability, fitness for a
  particular purpose and non-infringement, is not provided with this file. To
  the maximum extent permitted by applicable law, the authors or copyright
  holders shall not be liable for any claim, damages or other liabilities
  arising from the use of this file.

End of Terms

******************************************************************************/

use image::{
    ExtendedColorType, ImageEncoder,
    codecs::{jpeg::JpegEncoder, png::PngEncoder},
};
use lcms2::{Intent, PixelFormat, Profile, Transform};
use little_exif::{
    exif_tag::ExifTag,
    exif_tag_format::STRING,
    metadata::Metadata,
    rational::{iR64, uR64},
};
use num_cpus::get;
use rayon::{ThreadPoolBuilder, prelude::*};
use rsraw::RawImage;
use serde::Deserialize;
use std::{
    env,
    fs::{self, File, create_dir_all},
    io::{self, BufWriter, Read, Write},
    ops::Deref,
    path::{Path, PathBuf},
    u8,
};
use walkdir::WalkDir;

// ================= DEFAULT CONFIG =================

const DEFAULT_CONFIG: &str = include_str!("config.toml");

// ================= CONFIG =================

#[derive(Deserialize)]
struct Config {
    storage_root: String,
    icc: String,
    color_space: i32,
    outputs: Vec<OutputConfig>,
}

#[derive(Deserialize)]
struct OutputConfig {
    suffix: String,
    format: String,
    quality: Option<u8>,
    icc: Option<String>,
    subdir: Option<String>,
}

// ================= MAIN =================

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("COPYRIGHT 2026, Seho Lee.");

    let config = load_or_create_config()?;

    let args: Vec<String> = env::args().collect();
    if args.len() < 3 {
        println!("Usage: {} <name> <ext_of_raw_images>", args[0]);
        return Ok(());
    }

    let name = &args[1];
    let ext = &args[2];

    let base = PathBuf::from(&config.storage_root).join(name);
    create_dir_all(&base)?;

    let files: Vec<PathBuf> = WalkDir::new(".")
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .map_or(false, |e| e.eq_ignore_ascii_case(ext))
        })
        .map(|e| e.into_path())
        .collect();

    ThreadPoolBuilder::new()
        .num_threads(get())
        .build_global()
        .ok();

    files.par_iter().for_each(|f| {
        if let Err(err) = process_file(f, &base, &config) {
            println!("Error occurred when processing {}: {}", f.display(), err)
        }
    });

    Ok(())
}

// ================= CONFIG LOADER =================

fn load_or_create_config() -> Result<Config, Box<dyn std::error::Error>> {
    let home = dirs::home_dir().ok_or("No home directory")?;
    let cfg_dir = home.join(".nphotoman");
    let cfg_file = cfg_dir.join("config.toml");

    if !cfg_dir.exists() {
        create_dir_all(&cfg_dir)?;
    }

    if !cfg_file.exists() {
        let mut f = File::create(&cfg_file)?;
        f.write_all(DEFAULT_CONFIG.as_bytes())?;
        println!("Created config: {}", cfg_file.display());
    }

    let mut s = String::new();
    File::open(&cfg_file)?.read_to_string(&mut s)?;

    let config: Config = toml::from_str(&s)?;
    Ok(config)
}

// ================= CORE =================

fn process_file(
    raw_path: &Path,
    base: &Path,
    config: &Config,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("Processing {}", raw_path.display());

    // RAW decode
    let result: Result<Vec<u8>, io::Error> = fs::read(raw_path);
    if let Err(err) = result {
        return Result::Err(err.into());
    }
    let result = &result.unwrap();
    let result = RawImage::open(result);
    if let Err(err) = result {
        return Result::Err(err.into());
    }

    let mut raw = result.unwrap();
    raw.set_use_camera_wb(true);
    raw.set_use_camera_matrix(true);
    raw.as_mut().params.output_color = config.color_space;
    raw.unpack()?;

    let width = raw.width() as usize;
    let height = raw.height() as usize;
    let result = raw.process::<16>();
    if let Err(err) = result {
        return Result::Err(err.into());
    }

    let img = result.unwrap();
    let buf: Vec<u8> = img.deref().iter().flat_map(|e| e.to_ne_bytes()).collect();

    let icc_data_orig = fs::read(config.icc.clone())?;

    for out in &config.outputs {
        // ICC load
        let icc_data = if let Some(path) = &out.icc {
            Some(fs::read(path)?)
        } else {
            None
        };

        let mut nbuf = vec![0u8; buf.len()];
        if let Some(ref icc) = icc_data {
            let transform = Transform::new(
                &Profile::new_icc(&icc_data_orig)?,
                PixelFormat::RGB_16,
                &Profile::new_icc(icc)?,
                PixelFormat::RGB_16,
                Intent::Perceptual,
            )?;
            transform.transform_pixels(&buf, &mut nbuf);
        } else {
            nbuf.copy_from_slice(&buf);
        }

        let path_subdir: PathBuf;
        let dir: String;

        if let Some(subdir) = &out.subdir {
            path_subdir = raw_path.join(subdir);
            create_dir_all(&path_subdir)?;
            dir = path_subdir.to_str().unwrap().to_string();
        } else {
            dir = raw_path.to_str().unwrap().to_string();
        }

        match out.format.as_str() {
            "jpeg" => {
                let path = base.join(format!("{}-{}.jpeg", dir, out.suffix));
                let mut writer = BufWriter::new(File::create(&path)?);
                let mut enc = JpegEncoder::new_with_quality(&mut writer, out.quality.unwrap_or(90));

                if let Some(ref icc) = icc_data {
                    let _ = enc.set_icc_profile(icc.clone());
                } else {
                    let _ = enc.set_icc_profile(icc_data_orig.clone());
                }

                enc.set_exif_metadata(generate_exif(&raw)?)?;

                let mut nbuf8: Vec<u8> = nbuf
                    .chunks_exact(2)
                    .map(|e| (u16::from_ne_bytes([e[0], e[1]]) >> 8).try_into().unwrap())
                    .collect();

                enc.write_image(
                    nbuf8.by_ref(),
                    width.try_into().unwrap(),
                    height.try_into().unwrap(),
                    ExtendedColorType::Rgb8.into(),
                )?;
            }

            "png" => {
                let path = base.join(format!("{}-{}.png", dir, out.suffix));
                let mut enc = PngEncoder::new(File::create(&path)?);

                if let Some(ref icc) = icc_data {
                    let _ = enc.set_icc_profile(icc.clone());
                } else {
                    let _ = enc.set_icc_profile(icc_data_orig.clone());
                }

                enc.set_exif_metadata(generate_exif(&raw)?)?;

                enc.write_image(
                    nbuf.by_ref(),
                    width.try_into().unwrap(),
                    height.try_into().unwrap(),
                    ExtendedColorType::Rgb16.into(),
                )?;
            }

            _ => {}
        }
    }

    Ok(())
}

// ================= METADATA =================
fn generate_exif(image: &RawImage) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let info = image.full_info();
    let mut metadata = Metadata::new();

    if let Some(datetime) = info.datetime {
        metadata.set_tag(ExifTag::DateTimeOriginal(datetime.to_rfc3339()));
    }

    metadata.set_tag(ExifTag::ISOSpeed(vec![info.iso_speed]));
    metadata.set_tag(ExifTag::FNumber(vec![uR64::from(info.aperture as f64)]));
    metadata.set_tag(ExifTag::FocalLength(vec![uR64::from(
        info.focal_len as f64,
    )]));

    metadata.set_tag(ExifTag::ShutterSpeedValue(vec![iR64::from(
        info.shutter as f64,
    )]));

    metadata.set_tag(ExifTag::Make(info.make));
    metadata.set_tag(ExifTag::Model(info.model));

    metadata.set_tag(ExifTag::GPSLatitude(vec![uR64::from(
        info.gps.latitude[0] as f64
            + (info.gps.latitude[1] as f64 / 60f64)
            + (info.gps.latitude[2] as f64 / 3200f64),
    )]));

    metadata.set_tag(ExifTag::GPSLongitude(vec![uR64::from(
        info.gps.longitude[0] as f64
            + (info.gps.longitude[1] as f64 / 60f64)
            + (info.gps.longitude[2] as f64 / 3200f64),
    )]));

    metadata.set_tag(ExifTag::GPSAltitude(vec![uR64::from(
        info.gps.altitude as f64,
    )]));

    metadata.set_tag(ExifTag::Software(STRING::from("nphotoman")));

    Ok(metadata.encode()?)
}
