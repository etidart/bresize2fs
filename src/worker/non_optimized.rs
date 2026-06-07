/*
 * Copyright (C) 2026 Arseniy Astankov
 *
 * This file is part of bresize2fs.
 *
 * bresize2fs is free software: you can redistribute it and/or modify it
 * under the terms of the GNU General Public License as published by the Free
 * Software Foundation, either version 3 of the License, or (at your option)
 * any later version.
 *
 * bresize2fs is distributed in the hope that it will be useful, but WITHOUT
 * ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS
 * FOR A PARTICULAR PURPOSE. See the GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License along with
 * bresize2fs. If not, see <https://www.gnu.org/licenses/>.
 */

use anyhow::{Context, Result, anyhow};
use indicatif::{ProgressBar, ProgressState, ProgressStyle};
use std::{cmp::Ordering, fmt, fs::OpenOptions, io::{Read, Seek, SeekFrom, Write}, path::Path, process::Command};

use crate::{device::LoopDevice, fs::superblock::Superblock};

fn call_resize2fs(file: &Path, req_size_blks: u64) -> Result<()> {
    Command::new("resize2fs")
        .arg(file)
        .arg(req_size_blks.to_string())
        .status().map(|v| v.success()).context("failed to run resize2fs")
        .and_then(|v| v.then_some(()).ok_or(anyhow!("resize2fs returned non-successful status")))
}

fn move_data(file: &Path, from_sec: u64, to_sec: u64, count: u64) -> Result<()> {
    if from_sec == to_sec || count == 0 {
        return Ok(());
    }

    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(file)
        .context("failed to open the file for writing")?;

    let sector_size: u64 = 512;
    let start_offset = from_sec * sector_size;
    let dest_offset = to_sec * sector_size;
    let total_bytes = count * sector_size;

    let mut buf = [0u8; 4 * 1024 * 1024];

    let pb = ProgressBar::new(total_bytes);
    pb.set_style(ProgressStyle::with_template(
            "{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {bytes}/{total_bytes} {{eta}}"
        ).unwrap().with_key("eta", |state: &ProgressState, w: &mut dyn fmt::Write|
            write!(w, "{:.1}s", state.eta().as_secs_f64()).unwrap()
        ).progress_chars("#>-"));

    match from_sec.cmp(&to_sec) {
        Ordering::Less => {
            // moving right: copy from the end to avoid overwriting source
            let mut remaining = total_bytes;
            while remaining > 0 {
                let chunk = std::cmp::min(remaining as usize, buf.len());
                let offset = remaining - chunk as u64;

                if let Err(e) = file.seek(SeekFrom::Start(start_offset + offset))
                    .context("failed to seek the file") {
                    pb.abandon();
                    return Err(e);
                }
                if let Err(e) = file.read_exact(&mut buf[..chunk])
                    .context("failed to read the file") {
                    pb.abandon();
                    return Err(e);
                }

                if let Err(e) = file.seek(SeekFrom::Start(dest_offset + offset))
                    .context("failed to seek the file") {
                    pb.abandon();
                    return Err(e);
                }
                if let Err(e) = file.write_all(&buf[..chunk])
                    .context("failed to write to the file") {
                    pb.abandon();
                    return Err(e);
                }

                remaining -= chunk as u64;
                pb.set_position(total_bytes - remaining);
            }
        }
        Ordering::Greater => {
            // moving left: copy from the beginning to avoid overwriting source
            let mut processed = 0u64;
            while processed < total_bytes {
                let chunk = std::cmp::min((total_bytes - processed) as usize, buf.len());

                if let Err(e) = file.seek(SeekFrom::Start(start_offset + processed))
                    .context("failed to seek the file") {
                    pb.abandon();
                    return Err(e);
                }
                if let Err(e) = file.read_exact(&mut buf[..chunk])
                    .context("failed to read the file") {
                    pb.abandon();
                    return Err(e);
                }

                if let Err(e) = file.seek(SeekFrom::Start(dest_offset + processed))
                    .context("failed to seek the file") {
                    pb.abandon();
                    return Err(e);
                }
                if let Err(e) = file.write_all(&buf[..chunk])
                    .context("failed to write to the file") {
                    pb.abandon();
                    return Err(e);
                }

                processed += chunk as u64;
                pb.set_position(processed);
            }
        }
        Ordering::Equal => unreachable!(),
    }

    pb.finish_with_message("done");

    Ok(())
}

pub fn do_extend(file: &Path, f_offset: u64, f_sb: Superblock, c_value: u64) -> Result<()> {
    let total_size_secs = f_sb.block_count * f_sb.block_size as u64 / 512;

    assert_eq!(f_offset % 512, 0);

    move_data(file, f_offset / 512, f_offset / 512 - c_value,
        total_size_secs)?;

    let new_offset = (f_offset / 512 - c_value) * 512;

    let loop_d = match new_offset == 0 {
        true => None,
        false => Some(LoopDevice::new(file, new_offset).context("failed to create loop device")?)
    };

    let blks_to_extend = c_value / f_sb.block_size as u64;

    if blks_to_extend != 0 {
        call_resize2fs(match &loop_d {
            Some(v) => v.path(),
            None => file,
        }, f_sb.block_count + blks_to_extend)?;
    }

    Ok(())
}

pub fn do_shrink(file: &Path, f_offset: u64, f_sb: Superblock, c_value: u64) -> Result<()> {
    let total_size_secs = f_sb.block_count * f_sb.block_size as u64 / 512;
    let blks_to_shrink = c_value.div_ceil(f_sb.block_size as u64);

    let loop_d = match f_offset == 0 {
        true => None,
        false => Some(LoopDevice::new(file, f_offset).context("failed to create loop device")?)
    };

    call_resize2fs(match &loop_d {
        Some(v) => v.path(),
        None => file,
    }, f_sb.block_count - blks_to_shrink)?;

    assert_eq!(f_offset % 512, 0);

    move_data(file, f_offset / 512, f_offset / 512 + c_value,
        total_size_secs)?;

    Ok(())
}
