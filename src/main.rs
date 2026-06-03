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

use byte_unit::Byte;
use clap::Parser;
use anyhow::{Context, Result, anyhow, bail};
use indicatif::ProgressBar;
use inquire::Confirm;
use std::{cmp, ffi::OsStr, fs::File, io::{Read, Seek, SeekFrom}, num::NonZero, os::unix::ffi::OsStrExt};
use std::{path::{Path, PathBuf}, process::Command, str::FromStr};
use bstr::ByteSlice;

const MIN_FS_SIZE: u64 = 4096 * 32;

#[derive(Debug, Clone)]
enum Operation {
    ExtendToPartition,
    ExtendBySecs(u64),
    ShrinkBySecs(u64),
    SetSizeSecs(u64),
    ExtendByBlks(u64),
    ShrinkByBlks(u64),
    SetSizeBlks(u64),
}

impl FromStr for Operation {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == "" {
            return Ok(Self::ExtendToPartition);
        }

        let first_char = s.chars().next()
            .ok_or(anyhow!("string is empty"))?;
        let last_char = s.chars().last()
            .ok_or(anyhow!("string is empty"))?;

        let fidx: usize = match first_char.is_ascii_digit() {
            true => 0,
            false => 1,
        };
        let lidx: usize = match last_char.is_ascii_digit() {
            true => s.len(),
            false => s.len().saturating_sub(1),
        };

        let num: u64 = s.get(fidx..lidx)
            .ok_or(anyhow!("invalid string"))?
            .parse::<u64>().context("arg must be an integer")?
            .checked_mul(
                match last_char {
                    'K' | 'k' => 2,
                    'M' | 'm' => 2048,
                    'G' | 'g' => 2097152,
                    'T' | 't' => 2147483648,
                    _ => 1,
                }
            ).ok_or(anyhow!("too big size"))?;

        match last_char {
            'K' | 'k' | 'M' | 'm' | 'G' | 'g' | 'T' | 't' | 's' => {
                match first_char {
                    '+' => {
                        Ok(Self::ExtendBySecs(num))
                    },
                    '-' => {
                        Ok(Self::ShrinkBySecs(num))
                    },
                    c if c.is_ascii_digit() => {
                        Ok(Self::SetSizeSecs(num))
                    },
                    _ => {
                        Err(anyhow!("arg must start with +, - or a number"))
                    },
                }
            },
            c if c.is_ascii_digit() => {
                match first_char {
                    '+' => {
                        Ok(Self::ExtendByBlks(num))
                    },
                    '-' => {
                        Ok(Self::ShrinkByBlks(num))
                    },
                    c if c.is_ascii_digit() => {
                        Ok(Self::SetSizeBlks(num))
                    },
                    _ => {
                        Err(anyhow!("arg must start with +, - or a number"))
                    },
                }
            },
            _ => {
                Err(anyhow!("arg must end with K, M, G, T, s or a number"))
            }
        }
    }
}

#[derive(Parser)]
struct Args {
    path: PathBuf,
    #[arg(default_value = "")] // "" == ExtendToPartition
    op: Operation,
    #[arg(long)]
    offset: Option<u64>,
    #[arg(short)]
    yes: bool,
}

struct Superblock {
    block_size: u32,
    block_count: u64,
    uuid: String,
}

impl Superblock {
    fn parse(buf: &[u8]) -> Self {
        assert_eq!(buf.len(), 1024);
        let log_b_size = u32::from_le_bytes(buf[24..28].try_into().unwrap());
        assert!(log_b_size <= 6);
        let block_size = 2u32.pow(log_b_size);
        let incompat_features = u32::from_le_bytes(buf[96..100].try_into().unwrap());
        let mut block_count = u32::from_le_bytes(buf[4..8].try_into().unwrap()) as u64;
        if incompat_features & 64 > 0 {
            let mut app_bc = u32::from_le_bytes(buf[336..340].try_into().unwrap()) as u64;
            app_bc <<= 32;
            block_count |= app_bc;
        };
        let uuid_bytes = &buf[104..120];
        let uuid = format!("{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            uuid_bytes[0], uuid_bytes[1], uuid_bytes[2], uuid_bytes[3],
            uuid_bytes[4], uuid_bytes[5],
            uuid_bytes[6], uuid_bytes[7],
            uuid_bytes[8], uuid_bytes[9],
            uuid_bytes[10], uuid_bytes[11], uuid_bytes[12], uuid_bytes[13], uuid_bytes[14], uuid_bytes[15]
        );
        Self { block_size, block_count, uuid }
    }
}

enum VerifyError {
    NotValidFS,
    ValidFSErrors,
    MiscError(anyhow::Error),
}

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

struct FileWPath {
    file: File,
    path: PathBuf,
}

impl FileWPath {
    fn open(path: PathBuf) -> std::io::Result<Self> {
        let file = File::open(&path)?;
        std::io::Result::Ok(Self {
            file,
            path,
        })
    }
}

fn verify_offset(fs: &mut FileWPath, offset: u64) -> Result<Superblock, VerifyError> {
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

enum SearchError {
    NotFound,
    FoundErrors(u64),
    MiscError(anyhow::Error),
}

fn search_offset(fs: &mut FileWPath) -> Result<(u64, Superblock), SearchError> {
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

enum BasicOperation {
    Nothing,
    InvalidOp,
    ShrinkSecs(NonZero<u64>),
    ExtendSecs(NonZero<u64>),
}

impl BasicOperation {
    fn new(op: Operation, blk_size: u32, blk_count: u64, offset: u64) -> Self {
        assert_eq!(offset % 512, 0);
        match op {
            Operation::ExtendToPartition => {
                let res = NonZero::new(offset / 512);
                match res {
                    Some(x) => Self::ExtendSecs(x),
                    None => Self::Nothing,
                }
            },
            Operation::ExtendBySecs(v) => {
                let res = NonZero::new(cmp::min(offset / 512, v));
                match res {
                    Some(x) => Self::ExtendSecs(x),
                    None => Self::Nothing,
                }
            },
            Operation::ShrinkBySecs(v) => {
                let res = NonZero::new(
                    cmp::min((blk_count * blk_size as u64).checked_sub(MIN_FS_SIZE).unwrap() / 512, v)
                );
                match res {
                    Some(x) => Self::ShrinkSecs(x),
                    None => Self::Nothing,
                }
            },
            Operation::SetSizeSecs(v) => {
                let fs_size_sec = blk_size as u64 * blk_count / 512;
                match v.cmp(&fs_size_sec) {
                    cmp::Ordering::Less => {
                        if v < MIN_FS_SIZE / 512 {
                            Self::InvalidOp
                        } else {
                            let res = NonZero::new(fs_size_sec - v);
                            match res {
                                Some(x) => Self::ShrinkSecs(x),
                                None => Self::Nothing,
                            }
                        }
                    },
                    cmp::Ordering::Equal => Self::Nothing,
                    cmp::Ordering::Greater => {
                        let gr_by = v - fs_size_sec;
                        if gr_by > offset / 512 {
                            Self::InvalidOp
                        } else {
                            let res = NonZero::new(gr_by);
                            match res {
                                Some(x) => Self::ExtendSecs(x),
                                None => Self::Nothing,
                            }
                        }
                    },
                }
            },
            Operation::ExtendByBlks(v) => {
                Self::new(Operation::ExtendBySecs(v * blk_size as u64 / 512), blk_size, blk_count, offset)
            },
            Operation::ShrinkByBlks(v) => {
                Self::new(Operation::ShrinkBySecs(v * blk_size as u64 / 512), blk_size, blk_count, offset)
            },
            Operation::SetSizeBlks(v) => {
                Self::new(Operation::SetSizeSecs(v * blk_size as u64 / 512), blk_size, blk_count, offset)
            },
        }
    }
}

fn is_optimized_shrink_possible(c_value: u64, blk_size: u32) -> bool {
    todo!()
}

fn is_optimized_extend_possible(c_value: u64, blk_size: u32) -> bool {
    todo!()
}

fn main() -> Result<()> {
    let args = Args::parse();

    /*
    if offset available, verify it in 2 stages: manually check the magic and
    some fields (inodes count, block_size), then use fsck. print collected data
    and ask the user if everything is good and we can continue

    if offset is not available, search through the whole file for magic,
    move forward by 512 bytes, verify on each step for magic + some fields.
    if found something good, check with fsck, then print data and ask the user
    if fsck failed but filesystem found, notify the user. print the offset there!!

    get block size

    convert operation into either extend or shrink, the value type is 512 sectors

    check if the operation is possible the optimized way
    if yes -> do that
    if no -> ask the user if he wants to do that non-optimized way -> do that
     */

    let mut f_file = FileWPath::open(args.path)
        .context("could not open the fs file")?;

    let (f_offset, f_sb) = match args.offset {
        Some(v) => {
            match verify_offset(&mut f_file, v) {
                Ok(sb) => {
                    (v, sb)
                },
                Err(e) => {
                    match e {
                        VerifyError::NotValidFS =>
                            bail!("there is no valid e2fs on offset {}", v),
                        VerifyError::ValidFSErrors =>
                            bail!("valid filesystem found but it is not clean. use e2fsck"),
                        VerifyError::MiscError(error) =>
                            return Err(error.context("failed to run verification")),
                    }
                },
            }
        },
        None => {
            match search_offset(&mut f_file) {
                Ok(v) => v,
                Err(e) => {
                    match e {
                        SearchError::NotFound =>
                            bail!("valid e2fs was not found"),
                        SearchError::FoundErrors(vx) =>
                            bail!("valid filesystem found on offset {} but it is not clean. use e2fsck", vx),
                        SearchError::MiscError(error) =>
                            return Err(error.context("fail occured during the search")),
                    }
                },
            }
        },
    };

    println!("Found filesystem on offset {} with UUID={} of size {} ({} blocks)",
        f_offset,
        f_sb.uuid,
        Byte::from_u64(f_sb.block_count * f_sb.block_size as u64)
            .get_appropriate_unit(byte_unit::UnitType::Binary),
        Byte::from_u64(f_sb.block_size as u64)
            .get_appropriate_unit(byte_unit::UnitType::Binary)
    );

    if !args.yes {
        let ans = Confirm::new("Proceed?")
            .with_default(true)
            .prompt().context("failed to ask user")?;
        if !ans {
            return Ok(())
        }
    }

    let op = BasicOperation::new(args.op, f_sb.block_size, f_sb.block_count, f_offset);

    match op {
        BasicOperation::Nothing => println!("There is nothing to be done. Exiting.."),
        BasicOperation::InvalidOp => println!("Invalid operation. Exiting.."),
        BasicOperation::ShrinkSecs(v) => {
            let v = v.get();
            if is_optimized_shrink_possible(v, f_sb.block_size) {
                todo!()
            } else {
                todo!()
            }
        },
        BasicOperation::ExtendSecs(v) => {
            let v = v.get();
            if is_optimized_extend_possible(v, f_sb.block_size) {
                todo!()
            } else {
                todo!()
            }
        },
    }

    Ok(())
}
