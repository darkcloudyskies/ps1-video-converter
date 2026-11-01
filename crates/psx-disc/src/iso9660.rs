//! Just enough ISO9660 to map sector ranges to file names, so streams can be
//! named after the `.STR`/`.XA` files they live in.

use crate::error::{Error, Result};
use crate::source::SectorSource;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IsoFile {
    /// Full path with `/` separators and the `;1` version suffix removed.
    pub path: String,
    pub start_lba: u32,
    /// Number of sectors, computed as size / 2048 rounded up. For XA/STR files
    /// that hold Form 2 sectors the directory size still counts 2048 bytes per
    /// sector, so this is the right sector count for both.
    pub sector_count: u32,
    pub size: u32,
}

impl IsoFile {
    pub fn contains(&self, lba: u32) -> bool {
        lba >= self.start_lba && lba < self.start_lba + self.sector_count.max(1)
    }
}

struct DirRecord {
    lba: u32,
    size: u32,
    is_dir: bool,
    name: String,
}

fn parse_record(b: &[u8]) -> Option<DirRecord> {
    let len = *b.first()? as usize;
    if len < 34 || len > b.len() {
        return None;
    }
    let lba = u32::from_le_bytes(b[2..6].try_into().ok()?);
    let size = u32::from_le_bytes(b[10..14].try_into().ok()?);
    let flags = b[25];
    let name_len = b[32] as usize;
    let raw = b.get(33..33 + name_len)?;
    let name = match raw {
        [0] => ".".to_string(),
        [1] => "..".to_string(),
        _ => {
            let s = String::from_utf8_lossy(raw);
            s.split(';').next().unwrap_or("").to_string()
        }
    };
    Some(DirRecord {
        lba,
        size,
        is_dir: flags & 0x02 != 0,
        name,
    })
}

/// Walk the filesystem whose volume descriptors start at `base_lba + 16`.
/// Returns files sorted by start sector.
pub fn read_files(src: &mut dyn SectorSource, base_lba: u32) -> Result<Vec<IsoFile>> {
    let pvd = src.read_sector(base_lba + 16)?;
    let d = pvd.user_data();
    if d.len() < 2048 || d[0] != 1 || &d[1..6] != b"CD001" {
        return Err(Error::Iso(
            "no primary volume descriptor at sector 16".into(),
        ));
    }
    let root = parse_record(&d[156..190]).ok_or_else(|| Error::Iso("bad root record".into()))?;

    let mut files = Vec::new();
    let mut visited = HashSet::new();
    let mut stack = vec![(String::new(), root.lba, root.size)];
    while let Some((prefix, lba, size)) = stack.pop() {
        if !visited.insert(lba) {
            continue;
        }
        let n = size.div_ceil(2048);
        for s in src.read_range(base_lba + lba, n)? {
            let data = s.user_data();
            let data = &data[..data.len().min(2048)];
            let mut off = 0;
            while off < data.len() {
                if data[off] == 0 {
                    break; // rest of this sector is padding
                }
                let Some(rec) = parse_record(&data[off..]) else {
                    break;
                };
                off += data[off] as usize;
                if rec.name == "." || rec.name == ".." {
                    continue;
                }
                let path = format!("{prefix}/{}", rec.name);
                if rec.is_dir {
                    stack.push((path, rec.lba, rec.size));
                } else {
                    files.push(IsoFile {
                        path,
                        start_lba: base_lba + rec.lba,
                        sector_count: rec.size.div_ceil(2048),
                        size: rec.size,
                    });
                }
            }
        }
    }
    files.sort_by_key(|f| f.start_lba);
    Ok(files)
}

/// Look up the file containing `lba` in a list sorted by start sector.
pub fn file_at(files: &[IsoFile], lba: u32) -> Option<usize> {
    let i = files.partition_point(|f| f.start_lba <= lba);
    // Files may be nested oddly on some discs; check a few candidates back.
    (i.saturating_sub(4)..i)
        .rev()
        .find(|&j| files[j].contains(lba))
}

/// Helpers for building synthetic discs in tests.
#[doc(hidden)]
pub mod testutil {
    use crate::sector::{RawSector, SubHeader, SubMode};
    use crate::source::MemoryDisc;

    fn record(name: &[u8], lba: u32, size: u32, dir: bool) -> Vec<u8> {
        let mut r = vec![0u8; 33 + name.len() + (1 - name.len() % 2)];
        r[0] = r.len() as u8;
        r[2..6].copy_from_slice(&lba.to_le_bytes());
        r[6..10].copy_from_slice(&lba.to_be_bytes());
        r[10..14].copy_from_slice(&size.to_le_bytes());
        r[14..18].copy_from_slice(&size.to_be_bytes());
        r[25] = if dir { 2 } else { 0 };
        r[32] = name.len() as u8;
        r[33..33 + name.len()].copy_from_slice(name);
        r
    }

    /// Build a tiny disc: root dir at 18 with MOVIE/ (dir at 19) holding
    /// OPEN.STR at 30 (40 sectors), plus SYSTEM.CNF at 25.
    pub fn tiny_disc(total: u32) -> MemoryDisc {
        let sh = SubHeader {
            submode: SubMode(SubMode::DATA),
            ..Default::default()
        };
        let mut sectors: Vec<RawSector> = (0..total)
            .map(|l| RawSector::build_mode2(l, sh, &[]))
            .collect();
        let mut pvd = vec![0u8; 2048];
        pvd[0] = 1;
        pvd[1..6].copy_from_slice(b"CD001");
        let root = record(&[0], 18, 2048, true);
        pvd[156..156 + root.len()].copy_from_slice(&root);
        sectors[16] = RawSector::build_mode2(16, sh, &pvd);

        let mut rootdir = Vec::new();
        rootdir.extend(record(&[0], 18, 2048, true));
        rootdir.extend(record(&[1], 18, 2048, true));
        rootdir.extend(record(b"MOVIE", 19, 2048, true));
        rootdir.extend(record(b"SYSTEM.CNF;1", 25, 60, false));
        sectors[18] = RawSector::build_mode2(18, sh, &rootdir);

        let mut movie = Vec::new();
        movie.extend(record(&[0], 19, 2048, true));
        movie.extend(record(&[1], 18, 2048, true));
        movie.extend(record(b"OPEN.STR;1", 30, 40 * 2048, false));
        sectors[19] = RawSector::build_mode2(19, sh, &movie);
        MemoryDisc { sectors }
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::tiny_disc;
    use super::*;

    #[test]
    fn walks_tree() {
        let mut disc = tiny_disc(80);
        let files = read_files(&mut disc, 0).unwrap();
        assert_eq!(
            files,
            vec![
                IsoFile {
                    path: "/SYSTEM.CNF".into(),
                    start_lba: 25,
                    sector_count: 1,
                    size: 60
                },
                IsoFile {
                    path: "/MOVIE/OPEN.STR".into(),
                    start_lba: 30,
                    sector_count: 40,
                    size: 40 * 2048
                },
            ]
        );
        assert_eq!(file_at(&files, 35), Some(1));
        assert_eq!(file_at(&files, 26), None);
        assert_eq!(file_at(&files, 10), None);
    }
}
