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

use clap::Parser;
use anyhow::{Context, Result, anyhow};
use std::{cmp, num::NonZero, path::PathBuf, str::FromStr};

pub const MIN_FS_SIZE: u64 = 4096 * 32;

#[derive(Debug, Clone)]
pub enum Operation {
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

pub enum BasicOperation {
    Nothing,
    InvalidOp,
    ShrinkSecs(NonZero<u64>),
    ExtendSecs(NonZero<u64>),
}

impl BasicOperation {
    pub fn new(op: Operation, blk_size: u32, blk_count: u64, offset: u64) -> Self {
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

#[derive(Parser)]
pub struct Args {
    pub path: PathBuf,
    #[arg(default_value = "")] // "" == ExtendToPartition
    pub op: Operation,
    #[arg(long)]
    pub offset: Option<u64>,
    #[arg(short)]
    pub yes: bool,
}

