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

use anyhow::{Context, Result, bail};
use indicatif::ProgressBar;
use std::{ffi::OsStr, fs::File, io::{Read, Seek, SeekFrom}, os::unix::ffi::OsStrExt};
use std::{path::{Path, PathBuf}, process::Command};
use bstr::ByteSlice;

use crate::cli::MIN_FS_SIZE;
use crate::fs::superblock::Superblock;

struct LoopDevice {
    device: PathBuf,
}

fn trim_bytes(mut bytes: &[u8]) -> &[u8] {
    while let Some((first, rest)) = bytes.split_first() {
        if first.is_ascii_whitespace() {
            bytes = rest;
        } else {
            break;
        }
    }
    while let Some((last, rest)) = bytes.split_last() {
        if last.is_ascii_whitespace() {
            bytes = rest;
        } else {
            break;
        }
    }
    bytes
}

impl LoopDevice {
    fn new(file_path: &Path, offset: u64) -> Result<Self> {
        let mut cmd = Command::new("losetup");
        cmd.args(["--find", "--show", "--offset", &offset.to_string()]);
        cmd.arg(file_path);

        let output = cmd.output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("losetup failed: {}", stderr);
        }
        let device = PathBuf::from(
            OsStr::from_bytes(trim_bytes(&output.stdout))
        );
        Ok(Self { device })
    }

    fn path(&self) -> &Path {
        &self.device
    }
}

impl Drop for LoopDevice {
    fn drop(&mut self) {
        if let Err(e) = || -> Result<()> {
            let status = Command::new("losetup")
                .arg("-d")
                .arg(&self.device)
                .status()?;
            if !status.success() {
                bail!("losetup failed");
            }
            Ok(())
        }() {
            eprintln!("Warning: failed to detach {}: {}", self.device.to_string_lossy(), e);
        }
    }
}

pub struct FileWPath {
    file: File,
    path: PathBuf,
}

impl FileWPath {
    pub fn open(path: PathBuf) -> std::io::Result<Self> {
        let file = File::open(&path)?;
        std::io::Result::Ok(Self {
            file,
            path,
        })
    }
}

pub enum VerifyError {
    NotValidFS,
    ValidFSErrors,
    MiscError(anyhow::Error),
}

pub fn verify_offset(fs: &mut FileWPath, offset: u64) -> Result<Superblock, VerifyError> {
    let mut buf = [0u8; 1024];
    fs.file.seek(SeekFrom::Start(offset + 1024))
        .context("could not seek fs file")
        .map_err(|v| VerifyError::MiscError(v))?;
    fs.file.read_exact(&mut buf).context("could not read fs file")
        .map_err(|v| VerifyError::MiscError(v))?;

    if buf[56..58] != [0x53, 0xef] {
        return Err(VerifyError::NotValidFS)
    }

    let log_block_size = u32::from_le_bytes(buf[24..28].try_into().unwrap());
    if log_block_size > 6 {
        return Err(VerifyError::NotValidFS);
    }

    let inodes_count = u32::from_le_bytes(buf[0..4].try_into().unwrap());
    if inodes_count == 0 {
        return Err(VerifyError::NotValidFS);
    }

    let loop_device = match offset {
        0 => None,
        _ => Some(LoopDevice::new(&fs.path, offset)
            .context("could not create loop device")
            .map_err(|v| VerifyError::MiscError(v))?)
    };

    let path_to_check = match &loop_device {
        None => &fs.path,
        Some(v) => v.path(),
    };

    let output = Command::new("e2fsck")
        .arg("-nf").arg(path_to_check).output()
        .context("failed to run e2fsck").map_err(|v| VerifyError::MiscError(v))?;

    if output.status.success() {
        Ok(Superblock::parse(&buf))
    } else {
        if output.stdout.contains_str("The superblock could not be read or does not") {
            Err(VerifyError::NotValidFS)
        } else {
            Err(VerifyError::ValidFSErrors)
        }
    }
}

pub enum SearchError {
    NotFound,
    FoundErrors(u64),
    MiscError(anyhow::Error),
}

pub fn search_offset(fs: &mut FileWPath) -> Result<(u64, Superblock), SearchError> {
    let f_size = fs.file.metadata()
        .context("could not get file size")
        .map_err(|e| SearchError::MiscError(e))?.len();

    let mut cur_offset = 0u64;

    match verify_offset(fs, cur_offset) {
        Ok(v) => return Ok((cur_offset, v)),
        Err(e) => {
            match e {
                VerifyError::NotValidFS => (),
                VerifyError::ValidFSErrors => return Err(SearchError::FoundErrors(cur_offset)),
                VerifyError::MiscError(error) =>
                    return Err(SearchError::MiscError(error.context("verify_offset failed"))),
            }
        },
    }

    let loops = f_size.checked_sub(cur_offset + MIN_FS_SIZE)
        .map(|diff| diff / 512 + 1)
        .unwrap_or(0);

    println!("Searching for the filesystem...");

    let pb = ProgressBar::new(loops);
    pb.inc(1);

    for _ in 1..loops {
        match verify_offset(fs, cur_offset) {
            Ok(v) => return {
                pb.inc(1);
                pb.finish_with_message("Done");
                Ok((cur_offset, v))
            },
            Err(e) => {
                match e {
                    VerifyError::NotValidFS => (),
                    VerifyError::ValidFSErrors => {
                        pb.finish_with_message("An error occured");
                        return Err(SearchError::FoundErrors(cur_offset))
                    },
                    VerifyError::MiscError(error) => {
                        pb.finish_with_message("An error occured");
                        return Err(SearchError::MiscError(error.context("verify_offset failed")))
                    },
                }
            },
        }
        pb.inc(1);
        cur_offset += 512;
    }

    Err(SearchError::NotFound)
}
