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

pub struct Superblock {
    pub block_size: u32,
    pub block_count: u64,
    pub uuid: String,
}

impl Superblock {
    pub fn parse(buf: &[u8]) -> Self {
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
