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

mod cli;
mod fs;
mod device;
mod analyzer;
mod worker;

use clap::Parser;
use byte_unit::Byte;
use anyhow::{Context, Result, bail};
use inquire::Confirm;

use cli::{Args, BasicOperation};
use device::{FileWPath, verify_offset, search_offset, VerifyError, SearchError};
use analyzer::{is_optimized_shrink_possible, is_optimized_extend_possible};
use worker::{non_optimized, optimized};

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

    drop(f_file.file);

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
                optimized::do_shrink()
            } else {
                non_optimized::do_shrink(&f_file.path, f_offset, f_sb, v)
                    .context("failed to do non-optimized shrink")?;
            }
        },
        BasicOperation::ExtendSecs(v) => {
            let v = v.get();
            if is_optimized_extend_possible(v, f_sb.block_size) {
                optimized::do_extend()
            } else {
                non_optimized::do_extend(&f_file.path, f_offset, f_sb, v)
                    .context("failed to do non-optimized extend")?;
            }
        },
    }

    // check fs using e2fsck
    todo!();

    Ok(())
}
