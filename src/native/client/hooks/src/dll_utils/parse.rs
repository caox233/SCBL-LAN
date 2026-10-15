use std::io;

use tracing::debug;
use tracing::instrument;

/// Represents the publisher fields used to identify a parsed DLL.
#[derive(Default, Debug)]
pub struct Dll {
    pub company: Option<String>,
    pub product: Option<String>,
}

/// A macro for implementing methods to read primitive types from a byte stream.
macro_rules! read_impl {
    ($ty:ident) => {
        fn $ty(&mut self) -> std::io::Result<$ty> {
            let mut tmp = [0u8; std::mem::size_of::<$ty>()];
            self.read_exact(&mut tmp)?;
            Ok($ty::from_le_bytes(tmp))
        }
    };
}

/// A trait for reading primitive types from a byte stream.
trait BytesRead
where
    Self: io::Read,
{
    fn u8(&mut self) -> std::io::Result<u8> {
        let mut tmp = [0u8; 1];
        self.read_exact(&mut tmp)?;
        Ok(tmp[0])
    }

    read_impl!(u16);
    read_impl!(u32);
    read_impl!(u64);
}

impl<T: AsRef<[u8]>> BytesRead for std::io::Cursor<T> {}

fn invalid_data(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn checked_range(data: &[u8], offset: usize, size: usize) -> io::Result<&[u8]> {
    let end = offset
        .checked_add(size)
        .ok_or_else(|| invalid_data("resource range overflow"))?;
    data.get(offset..end)
        .ok_or_else(|| invalid_data("resource range outside file"))
}

fn resource_tail(data: &[u8], offset: usize) -> io::Result<&[u8]> {
    data.get(offset..)
        .ok_or_else(|| invalid_data("resource directory offset outside section"))
}

fn read_utf16_z(cursor: &mut io::Cursor<&[u8]>) -> io::Result<String> {
    let mut units = Vec::new();
    loop {
        let value = cursor.u16()?;
        if value == 0 {
            break;
        }
        units.push(value);
    }
    String::from_utf16(&units).map_err(|_| invalid_data("invalid UTF-16 version string"))
}

fn align_dword(cursor: &mut io::Cursor<&[u8]>) -> io::Result<()> {
    while cursor.position() % 4 != 0 {
        cursor.u8()?;
    }
    Ok(())
}

// End-of-container is not the same as a truncated child. Each successful child
// consumes a nonzero declared length. Only trailing alignment padding is skipped.
fn has_child(cursor: &mut io::Cursor<&[u8]>) -> io::Result<bool> {
    let position =
        usize::try_from(cursor.position()).map_err(|_| invalid_data("cursor position overflow"))?;
    let remaining = cursor
        .get_ref()
        .get(position..)
        .ok_or_else(|| invalid_data("cursor outside version block"))?;
    if remaining.is_empty() {
        return Ok(false);
    }
    let padding = (4 - position % 4) % 4;
    if remaining.len() <= padding {
        if remaining.iter().any(|&byte| byte != 0) {
            return Err(invalid_data("invalid trailing version padding"));
        }
        cursor.set_position(cursor.get_ref().len() as u64);
        return Ok(false);
    }
    align_dword(cursor)?;
    Ok(true)
}

trait SubCursor {
    fn sub_cursor_from_length(&mut self) -> io::Result<io::Cursor<&[u8]>>;
}

impl SubCursor for io::Cursor<&[u8]> {
    fn sub_cursor_from_length(&mut self) -> io::Result<io::Cursor<&[u8]>> {
        let pos = usize::try_from(self.position())
            .map_err(|_| invalid_data("cursor position overflow"))?;
        let size = usize::from(self.u16()?);
        if size < 6 {
            return Err(invalid_data("version block shorter than its header"));
        }
        let end = pos
            .checked_add(size)
            .ok_or_else(|| invalid_data("version block length overflow"))?;
        if end > self.get_ref().len() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "truncated version block",
            ));
        }
        self.set_position(end as u64);
        Ok(io::Cursor::new(&self.get_ref()[pos..end]))
    }
}

/// Represents a named entry in a resource directory.
#[derive(Debug)]
#[allow(dead_code)]
struct NameEntry {
    name_offset: u32,
    offset: usize,
}

/// Represents an ID entry in a resource directory.
#[derive(Debug)]
struct IdEntry {
    offset: usize,
    id: u32,
}

/// Represents a resource directory in a PE file.
#[derive(Debug)]
struct Directory {
    #[allow(dead_code)]
    name_entries: Vec<NameEntry>,
    id_entries: Vec<IdEntry>,
}

impl Directory {
    /// Parses a resource directory from a byte stream.
    fn parse<R: BytesRead>(rdr: &mut R) -> io::Result<Self> {
        let _characteristics = rdr.u32()?;
        let _time_date_stamp = rdr.u32()?;
        let _major = rdr.u16()?;
        let _minor = rdr.u16()?;
        let name_entries_count = rdr.u16()?;
        let id_entries_count = rdr.u16()?;

        let name_entries: Vec<NameEntry> = (0..name_entries_count)
            .map(|_| {
                Ok(NameEntry {
                    name_offset: rdr.u32()?,
                    offset: (rdr.u32()? & 0x7fff_ffff) as usize,
                })
            })
            .collect::<io::Result<_>>()?;
        let id_entries: Vec<IdEntry> = (0..id_entries_count)
            .map(|_| {
                Ok(IdEntry {
                    id: rdr.u32()?,
                    offset: (rdr.u32()? & 0x7fff_ffff) as usize,
                })
            })
            .collect::<io::Result<_>>()?;

        Ok(Directory {
            name_entries,
            id_entries,
        })
    }
}

/// The resource type for version information.
const RT_VERSION: u32 = 0x10;

#[instrument(skip_all)]
fn parse_fixed_file_info(version_data: &mut io::Cursor<&[u8]>) -> io::Result<Dll> {
    let signature = version_data.u32()?;
    if signature != 0xFEEF_04BD {
        return Err(invalid_data("invalid fixed file information signature"));
    }
    let _struct_version = version_data.u32()?;
    let _file_version_minor = version_data.u16()?;
    let _file_version_major = version_data.u16()?;
    let _file_version_x = version_data.u16()?;
    let _file_version_patch = version_data.u16()?;
    let _product_version = version_data.u64()?;
    let _file_flags_mask = version_data.u64()?;
    let _file_flags = version_data.u32()?;
    let _file_os = version_data.u32()?;
    let _file_type = version_data.u32()?;
    let _file_sub_type = version_data.u32()?;
    let _file_timestamp = version_data.u32()?;

    Ok(Dll::default())
}

#[instrument(skip_all)]
fn parse_version(version_data: &mut io::Cursor<&[u8]>) -> io::Result<Dll> {
    let mut version_data = version_data.sub_cursor_from_length()?;
    let _length = version_data.u16()?;
    let _value_length = version_data.u16()?;
    let _type = version_data.u16()?;
    if read_utf16_z(&mut version_data)? != "VS_VERSION_INFO" {
        return Err(invalid_data("invalid VS_VERSION_INFO key"));
    }
    align_dword(&mut version_data)?;
    let mut dll = parse_fixed_file_info(&mut version_data)?;
    while has_child(&mut version_data)? {
        parse_version_entry(&mut version_data, &mut dll)?;
    }
    Ok(dll)
}

#[instrument(skip_all)]
fn parse_string(version_data: &mut io::Cursor<&[u8]>, dll: &mut Dll) -> io::Result<()> {
    let mut version_data = version_data.sub_cursor_from_length()?;
    let _length = version_data.u16()?;
    let value_length = version_data.u16()?;
    let _type = version_data.u16()?;
    let key = read_utf16_z(&mut version_data)?;
    align_dword(&mut version_data)?;
    let value = if value_length == 0 {
        String::new()
    } else {
        read_utf16_z(&mut version_data)?
    };
    debug!("{key}: {value}");
    if key == "CompanyName" {
        dll.company = Some(value);
    } else if key == "ProductName" {
        dll.product = Some(value);
    }
    Ok(())
}

#[instrument(skip_all)]
fn parse_string_table(version_data: &mut io::Cursor<&[u8]>, dll: &mut Dll) -> io::Result<()> {
    let mut version_data = version_data.sub_cursor_from_length()?;
    let _length = version_data.u16()?;
    let _value_length = version_data.u16()?;
    let _type = version_data.u16()?;
    let _key = read_utf16_z(&mut version_data)?;
    while has_child(&mut version_data)? {
        parse_string(&mut version_data, dll)?;
    }
    Ok(())
}

#[instrument(skip_all)]
fn parse_version_entry(version_data: &mut io::Cursor<&[u8]>, dll: &mut Dll) -> io::Result<()> {
    let mut version_data = version_data.sub_cursor_from_length()?;
    let _length = version_data.u16()?;
    let _value_length = version_data.u16()?;
    let _type = version_data.u16()?;
    let key = read_utf16_z(&mut version_data)?;
    align_dword(&mut version_data)?;
    // Preserve the existing selection of StringFileInfo's first string table.
    if key != "StringFileInfo" {
        return Ok(());
    }
    parse_string_table(&mut version_data, dll)
}

/// Parses a DLL file from a byte slice and returns a `Dll` struct.
pub fn parse(data: &[u8]) -> io::Result<Dll> {
    let dll = goblin::pe::PE::parse(data).map_err(|error| invalid_data(&error.to_string()))?;
    let rsrc = dll
        .sections
        .iter()
        .find(|section| &section.name == b".rsrc\0\0\0")
        .ok_or_else(|| invalid_data("resource section not found"))?;
    // Metadata must be backed by file bytes. Do not allocate a virtual-size
    // zero-filled image when inspecting a file's publisher information.
    let rsrc_data = checked_range(
        data,
        rsrc.pointer_to_raw_data as usize,
        rsrc.size_of_raw_data as usize,
    )?;
    let root_dir = Directory::parse(&mut io::Cursor::new(rsrc_data))?;
    let Some(entry) = root_dir
        .id_entries
        .iter()
        .find(|entry| entry.id == RT_VERSION)
    else {
        return Ok(Dll::default());
    };
    let entry_data = resource_tail(rsrc_data, entry.offset)?;
    let resource_dir = Directory::parse(&mut io::Cursor::new(entry_data))?;
    let version_entry = resource_dir
        .id_entries
        .first()
        .ok_or_else(|| invalid_data("version resource has no ID entry"))?;
    let resource_data = resource_tail(rsrc_data, version_entry.offset)?;
    let lang_dir = Directory::parse(&mut io::Cursor::new(resource_data))?;
    let lang_entry = lang_dir
        .id_entries
        .first()
        .ok_or_else(|| invalid_data("version resource has no language entry"))?;
    let mut version_data = io::Cursor::new(resource_tail(rsrc_data, lang_entry.offset)?);
    let rva = version_data.u32()? as usize;
    let size = version_data.u32()? as usize;
    let _codepage = version_data.u32()?;
    let _reserved = version_data.u32()?;
    let optional = dll
        .header
        .optional_header
        .as_ref()
        .ok_or_else(|| invalid_data("missing PE optional header"))?;
    let offset = goblin::pe::utils::find_offset(
        rva,
        &dll.sections,
        optional.windows_fields.file_alignment,
        &goblin::pe::options::ParseOptions::default(),
    )
    .ok_or_else(|| invalid_data("version RVA outside file sections"))?;
    parse_version(&mut io::Cursor::new(checked_range(data, offset, size)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn version(children: &[u8]) -> Vec<u8> {
        let mut data = vec![0, 0, 52, 0, 0, 0];
        for unit in "VS_VERSION_INFO".encode_utf16().chain(Some(0)) {
            data.extend(unit.to_le_bytes());
        }
        while data.len() % 4 != 0 {
            data.push(0);
        }
        data.extend(0xFEEF_04BDu32.to_le_bytes());
        data.extend([0; 48]);
        data.extend(children);
        let length = u16::try_from(data.len()).unwrap();
        data[..2].copy_from_slice(&length.to_le_bytes());
        data
    }
    #[test]
    fn file_ranges_check_bounds_and_addition_overflow() {
        assert_eq!(checked_range(&[1, 2, 3], 1, 2).unwrap(), &[2, 3]);
        assert!(checked_range(&[1, 2, 3], 2, 2).is_err());
        assert!(checked_range(&[], usize::MAX, 1).is_err());
        assert!(resource_tail(&[1, 2], 3).is_err());
    }
    #[test]
    fn malformed_pe_prefixes_return_errors() {
        for data in [vec![], vec![b'M'], b"MZ".to_vec(), vec![0; 64]] {
            assert!(parse(&data).is_err());
        }
    }
    #[test]
    fn valid_minimal_version_keeps_absent_publisher_fields() {
        let data = version(&[]);
        let parsed = parse_version(&mut io::Cursor::new(data.as_slice())).unwrap();
        assert!(parsed.company.is_none() && parsed.product.is_none());
    }
    #[test]
    fn invalid_version_signature_and_key_return_errors() {
        let mut data = version(&[]);
        data[40..44].fill(0);
        assert!(parse_version(&mut io::Cursor::new(data.as_slice())).is_err());
        let mut data = version(&[]);
        data[6] = b'X';
        assert!(parse_version(&mut io::Cursor::new(data.as_slice())).is_err());
    }
    #[test]
    fn truncated_and_zero_length_children_are_not_end_markers() {
        for child in [vec![0; 6], vec![255, 255, 0, 0, 0, 0], vec![6]] {
            let data = version(&child);
            assert!(parse_version(&mut io::Cursor::new(data.as_slice())).is_err());
        }
    }
    #[test]
    fn utf16_valid_pairs_empty_and_invalid_sequences() {
        let bytes: Vec<u8> = "海🌈"
            .encode_utf16()
            .chain(Some(0))
            .flat_map(u16::to_le_bytes)
            .collect();
        assert_eq!(
            read_utf16_z(&mut io::Cursor::new(bytes.as_slice())).unwrap(),
            "海🌈"
        );
        assert_eq!(read_utf16_z(&mut io::Cursor::new(&[0, 0][..])).unwrap(), "");
        assert!(read_utf16_z(&mut io::Cursor::new(&[0, 0xd8, 0, 0][..])).is_err());
        assert!(read_utf16_z(&mut io::Cursor::new(&[b'a', 0][..])).is_err());
    }
}
