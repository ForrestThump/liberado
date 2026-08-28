//! Small ustar writer for verified WebUI bundles.

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use flate2::Compression;
use flate2::write::GzEncoder;

pub(crate) fn pack_directory(source: &Path, archive: &Path) -> Result<(), String> {
    let file = File::create(archive)
        .map_err(|error| format!("create archive {}: {error}", archive.display()))?;
    let mut encoder = GzEncoder::new(file, Compression::default());
    let mut entries = Vec::new();
    collect_entries(source, &mut entries)?;
    entries.sort();
    for path in entries {
        write_entry(&mut encoder, source, &path)?;
    }
    encoder
        .write_all(&[0; 1024])
        .map_err(|error| format!("finish tar stream: {error}"))?;
    encoder
        .finish()
        .map_err(|error| format!("finish gzip archive: {error}"))?;
    Ok(())
}

fn collect_entries(directory: &Path, entries: &mut Vec<PathBuf>) -> Result<(), String> {
    for entry in std::fs::read_dir(directory)
        .map_err(|error| format!("read archive directory {}: {error}", directory.display()))?
    {
        let path = entry
            .map_err(|error| format!("read archive entry: {error}"))?
            .path();
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|error| format!("inspect archive entry {}: {error}", path.display()))?;
        if metadata.file_type().is_symlink() {
            return Err(format!(
                "WebUI bundle contains a symbolic link: {}",
                path.display()
            ));
        }
        entries.push(path.clone());
        if metadata.is_dir() {
            collect_entries(&path, entries)?;
        } else if !metadata.is_file() {
            return Err(format!(
                "WebUI bundle contains a non-file entry: {}",
                path.display()
            ));
        }
    }
    Ok(())
}

fn write_entry(writer: &mut impl Write, root: &Path, path: &Path) -> Result<(), String> {
    let metadata = std::fs::metadata(path)
        .map_err(|error| format!("inspect archive entry {}: {error}", path.display()))?;
    let mut name = path
        .strip_prefix(root)
        .map_err(|error| format!("make archive path relative: {error}"))?
        .to_string_lossy()
        .replace('\\', "/");
    if metadata.is_dir() {
        name.push('/');
    }
    let mut header = [0_u8; 512];
    write_name(&mut header, &name)?;
    write_octal(
        &mut header[100..108],
        if metadata.is_dir() { 0o755 } else { 0o644 },
    )?;
    write_octal(&mut header[108..116], 0)?;
    write_octal(&mut header[116..124], 0)?;
    write_octal(
        &mut header[124..136],
        if metadata.is_file() {
            metadata.len()
        } else {
            0
        },
    )?;
    write_octal(&mut header[136..148], 0)?;
    header[148..156].fill(b' ');
    header[156] = if metadata.is_dir() { b'5' } else { b'0' };
    header[257..263].copy_from_slice(b"ustar\0");
    header[263..265].copy_from_slice(b"00");
    let checksum: u64 = header.iter().map(|byte| u64::from(*byte)).sum();
    write_checksum(&mut header[148..156], checksum)?;
    writer
        .write_all(&header)
        .map_err(|error| format!("write archive header for {name}: {error}"))?;
    if metadata.is_file() {
        let mut file = File::open(path)
            .map_err(|error| format!("open archive entry {}: {error}", path.display()))?;
        std::io::copy(&mut file, writer)
            .map_err(|error| format!("write archive entry {name}: {error}"))?;
        let padding = (512 - metadata.len() % 512) % 512;
        writer
            .write_all(&vec![0; padding as usize])
            .map_err(|error| format!("pad archive entry {name}: {error}"))?;
    }
    Ok(())
}

fn write_name(header: &mut [u8; 512], name: &str) -> Result<(), String> {
    let bytes = name.as_bytes();
    if bytes.len() <= 100 {
        header[..bytes.len()].copy_from_slice(bytes);
        return Ok(());
    }
    let split = name
        .char_indices()
        .rfind(|(index, character)| {
            *character == '/'
                && *index <= 155
                && *index + 1 < bytes.len()
                && bytes.len() - index - 1 <= 100
        })
        .map(|(index, _)| index)
        .ok_or_else(|| format!("archive path is too long for ustar: {name}"))?;
    let (prefix, leaf) = name.split_at(split);
    let leaf = &leaf[1..];
    header[..leaf.len()].copy_from_slice(leaf.as_bytes());
    header[345..345 + prefix.len()].copy_from_slice(prefix.as_bytes());
    Ok(())
}

fn write_octal(field: &mut [u8], value: u64) -> Result<(), String> {
    let text = format!("{:0width$o}", value, width = field.len() - 1);
    if text.len() >= field.len() {
        return Err(format!("tar numeric field overflow: {value}"));
    }
    field[..text.len()].copy_from_slice(text.as_bytes());
    field[text.len()] = 0;
    Ok(())
}

fn write_checksum(field: &mut [u8], value: u64) -> Result<(), String> {
    let text = format!("{value:06o}");
    if text.len() != 6 {
        return Err(format!("tar checksum overflow: {value}"));
    }
    field[..6].copy_from_slice(text.as_bytes());
    field[6] = 0;
    field[7] = b' ';
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use super::*;

    #[test]
    fn packer_writes_regular_ustar_entries() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("public");
        std::fs::create_dir_all(source.join("assets")).unwrap();
        std::fs::write(source.join("index.html"), b"index").unwrap();
        std::fs::write(source.join("assets/app.wasm"), b"wasm").unwrap();
        let archive = temp.path().join("bundle.tar.gz");
        pack_directory(&source, &archive).unwrap();

        let mut bytes = Vec::new();
        flate2::read::GzDecoder::new(File::open(archive).unwrap())
            .read_to_end(&mut bytes)
            .unwrap();
        let mut names = Vec::new();
        let mut offset = 0;
        while bytes[offset..offset + 512].iter().any(|byte| *byte != 0) {
            let header = &bytes[offset..offset + 512];
            let end = header[..100]
                .iter()
                .position(|byte| *byte == 0)
                .unwrap_or(100);
            names.push(String::from_utf8(header[..end].to_vec()).unwrap());
            let end = header[124..136]
                .iter()
                .position(|byte| *byte == 0)
                .unwrap_or(12);
            let size =
                usize::from_str_radix(std::str::from_utf8(&header[124..124 + end]).unwrap(), 8)
                    .unwrap();
            offset += 512 + size.div_ceil(512) * 512;
        }
        assert_eq!(names, ["assets/", "assets/app.wasm", "index.html"]);
    }
}
