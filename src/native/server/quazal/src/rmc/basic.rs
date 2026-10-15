/// This module provides basic functionalities for reading and writing data to/from streams.
use std::collections::HashMap;
use std::ffi::CString;
use std::io::Cursor;
use std::io::{self};

use byteorder::LittleEndian;
use byteorder::ReadBytesExt;
use byteorder::WriteBytesExt;
use derive_more::Display;
use derive_more::Error;
use derive_more::From;

// PRUDP reassembly is bounded below 256 KiB. These limits remain deliberately
// larger than normal game messages while preventing corrupted u32 prefixes from
// requesting multi-gigabyte allocations.
const MAX_LENGTH_PREFIXED_BYTES: usize = 1024 * 1024;
const MAX_COLLECTION_ITEMS: usize = 65_536;

/// Errors that can occur when reading from a stream.
#[derive(Error, Display, Debug, From)]
pub enum FromStreamError {
    /// An I/O error occurred.
    IO(#[error(source)] std::io::Error),
    /// Not enough bytes were available in the stream.
    #[display("{_0} bytes missing")]
    MissingBytes(#[error(ignore)] usize),
    /// An error occurred while parsing an integer.
    ParseInt(#[error(source)] std::num::ParseIntError),
    /// An error occurred while parsing a `StationURL`.
    ParseStationURL(#[error(source)] super::types::StationURLParseError),
    /// An error occurred while parsing a `CString`.
    ParseCString(#[error(source)] std::ffi::FromVecWithNulError),
    /// An error occurred during UTF-8 conversion.
    Utf8(#[error(source)] std::str::Utf8Error),
    /// A length prefix exceeded the defensive protocol limit.
    #[display("Length {_0} exceeds limit {_1}")]
    #[from(ignore)]
    LengthLimitExceeded(#[error(ignore)] usize, #[error(ignore)] usize),
    /// A bounded allocation could not be reserved.
    #[display("Allocation of {_0} entries failed")]
    #[from(ignore)]
    AllocationFailed(#[error(ignore)] usize),
    /// A polymorphic RMC value named a class that this server does not support.
    #[display("Unknown RMC class {_0}")]
    #[from(ignore)]
    UnknownClass(#[error(ignore)] String),
}

/// Errors that can occur when writing to a stream.
#[derive(Error, Display, Debug, From)]
pub enum ToStreamError {
    /// An I/O error occurred.
    IO(#[error(source)] std::io::Error),
}

/// A reader for streams, providing methods to read various data types.
/// It wraps a `ReadBytesExt` implementor and provides convenient methods
/// for reading primitive types, length-prefixed buffers, and custom `FromStream` types.
pub struct ReadStream<R: ReadBytesExt> {
    rdr: R,
}

/// Macro to generate `ReadStream` methods for reading numeric types.
/// It automatically handles little-endian byte order.
macro_rules! read_stream_num {
    ($i: ident, $f: ident) => {
        pub fn $i(&mut self) -> std::result::Result<$i, FromStreamError> {
            Ok(self.rdr.$f::<LittleEndian>()?)
        }
    };
}

impl<R: ReadBytesExt> ReadStream<R> {
    /// Creates a new `ReadStream` from a reader.
    pub fn from_reader(rdr: R) -> Self {
        Self { rdr }
    }

    /// Reads a `u8` from the stream.
    pub fn u8(&mut self) -> std::result::Result<u8, FromStreamError> {
        Ok(self.rdr.read_u8()?)
    }
    read_stream_num!(u16, read_u16);
    read_stream_num!(u32, read_u32);
    read_stream_num!(u64, read_u64);

    /// Reads an `i8` from the stream.
    pub fn i8(&mut self) -> std::result::Result<i8, FromStreamError> {
        Ok(self.rdr.read_i8()?)
    }

    read_stream_num!(i16, read_i16);
    read_stream_num!(i32, read_i32);
    read_stream_num!(i64, read_i64);

    read_stream_num!(f32, read_f32);
    read_stream_num!(f64, read_f64);

    /// Reads a boolean from the stream.
    pub fn bool(&mut self) -> std::result::Result<bool, FromStreamError> {
        self.u8().map(|b| b != 0)
    }

    /// Reads `l` bytes from the stream.
    pub fn read_n_bytes(&mut self, l: usize) -> std::result::Result<Vec<u8>, FromStreamError> {
        if l > MAX_LENGTH_PREFIXED_BYTES {
            return Err(FromStreamError::LengthLimitExceeded(l, MAX_LENGTH_PREFIXED_BYTES));
        }
        let mut buf = Vec::new();
        buf.try_reserve_exact(l).map_err(|_| FromStreamError::AllocationFailed(l))?;
        buf.resize(l, 0);
        match self.rdr.read_exact(&mut buf) {
            Ok(()) => Ok(buf),
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Err(FromStreamError::MissingBytes(l)),
            Err(e) => Err(FromStreamError::IO(e)),
        }
    }

    /// Reads a `u8` length-prefixed byte buffer from the stream.
    pub fn buf_u8(&mut self) -> std::result::Result<Vec<u8>, FromStreamError> {
        let l = self.u8()?;
        self.read_n_bytes(l as usize)
    }

    /// Reads a `u16` length-prefixed byte buffer from the stream.
    pub fn buf_u16(&mut self) -> std::result::Result<Vec<u8>, FromStreamError> {
        let l = self.u16()?;
        self.read_n_bytes(l as usize)
    }

    /// Reads a `u32` length-prefixed byte buffer from the stream.
    pub fn buf_u32(&mut self) -> std::result::Result<Vec<u8>, FromStreamError> {
        let l = self.u32()?;
        self.read_n_bytes(l as usize)
    }

    /// Reads a `FromStream` implementor from the stream.
    pub fn read<F>(&mut self) -> std::result::Result<F, FromStreamError>
    where
        F: FromStream,
    {
        <F as FromStream>::from_stream(self)
    }

    /// Reads all remaining bytes from the stream.
    pub fn read_all(&mut self) -> std::result::Result<Vec<u8>, FromStreamError> {
        let mut buf = vec![];
        self.rdr.read_to_end(&mut buf)?;
        Ok(buf)
    }
}

impl<T: AsRef<[u8]>> ReadStream<Cursor<T>> {
    /// Creates a new `ReadStream` from a byte slice.
    pub fn from_bytes(data: T) -> Self {
        Self::from_reader(Cursor::new(data))
    }
}

/// Trait to read a type from a stream or bytes in the Quazal encoding.
pub trait FromStream {
    /// Reads a single instance from the given stream.
    fn from_stream<R>(_stream: &mut ReadStream<R>) -> std::result::Result<Self, FromStreamError>
    where
        R: ReadBytesExt,
        Self: Sized;

    /// Reads a single instance from the given bytes. Doesn't fail if there are unused trailing bytes.
    fn from_bytes(data: &[u8]) -> std::result::Result<Self, FromStreamError>
    where
        Self: Sized,
    {
        Self::from_stream(&mut ReadStream::from_bytes(data))
    }
}

/// A writer for streams, providing methods to write various data types.
/// It wraps a `WriteBytesExt` implementor and provides convenient methods
/// for writing primitive types, length-prefixed buffers, and custom `ToStream` types.
pub struct WriteStream<W: WriteBytesExt> {
    wtr: W,
}

/// Macro to generate `WriteStream` methods for writing numeric types.
/// It automatically handles little-endian byte order.
macro_rules! write_stream_num {
    ($i: ident, $f: ident) => {
        pub fn $i(&mut self, value: $i) -> io::Result<usize> {
            self.wtr.$f::<LittleEndian>(value).map(|()| ::std::mem::size_of::<$i>())
        }
    };
}

impl<W: WriteBytesExt> WriteStream<W> {
    /// Creates a new `WriteStream` from a writer.
    pub fn from_writer(wtr: W) -> Self {
        Self { wtr }
    }

    /// Writes a `u8` to the stream.
    pub fn u8(&mut self, value: u8) -> io::Result<usize> {
        self.wtr.write_u8(value).map(|()| 1)
    }
    write_stream_num!(u16, write_u16);
    write_stream_num!(u32, write_u32);
    write_stream_num!(u64, write_u64);

    /// Writes an `i8` to the stream.
    pub fn i8(&mut self, value: i8) -> io::Result<usize> {
        self.wtr.write_i8(value).map(|()| 1)
    }

    write_stream_num!(i16, write_i16);
    write_stream_num!(i32, write_i32);
    write_stream_num!(i64, write_i64);

    write_stream_num!(f32, write_f32);
    write_stream_num!(f64, write_f64);

    /// Writes a boolean to the stream.
    pub fn bool(&mut self, value: bool) -> io::Result<usize> {
        self.u8(u8::from(value))
    }

    /// Writes a byte slice to the stream.
    pub fn write_n_bytes<T: AsRef<[u8]>>(&mut self, data: T) -> io::Result<usize> {
        self.wtr.write_all(data.as_ref()).map(|()| data.as_ref().len())
    }

    /// Writes a `u8` length-prefixed byte buffer to the stream.
    pub fn buf_u8<T: AsRef<[u8]>>(&mut self, data: T) -> io::Result<usize> {
        let d = data.as_ref();
        let len = u8::try_from(d.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "buffer length exceeds u8"))?;
        Ok(self.u8(len)? + self.write_n_bytes(d)?)
    }

    /// Writes a `u16` length-prefixed byte buffer to the stream.
    pub fn buf_u16<T: AsRef<[u8]>>(&mut self, data: T) -> io::Result<usize> {
        let d = data.as_ref();
        let len = u16::try_from(d.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "buffer length exceeds u16"))?;
        Ok(self.u16(len)? + self.write_n_bytes(d)?)
    }

    /// Writes a `u32` length-prefixed byte buffer to the stream.
    pub fn buf_u32<T: AsRef<[u8]>>(&mut self, data: T) -> io::Result<usize> {
        let d = data.as_ref();
        let len = u32::try_from(d.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "buffer length exceeds u32"))?;
        Ok(self.u32(len)? + self.write_n_bytes(d)?)
    }

    /// Writes a `ToStream` implementor to the stream.
    pub fn write<T>(&mut self, value: &T) -> io::Result<usize>
    where
        T: ToStream,
    {
        value.to_stream(self)
    }
}

/// Trait to write a type to a stream or bytes.
pub trait ToStream {
    /// Writes the instance to the given stream. Returns the amount of bytes written.
    fn to_stream<W>(&self, _stream: &mut WriteStream<W>) -> io::Result<usize>
    where
        W: WriteBytesExt;

    /// Encodes checked wire data, propagating write errors without publishing partial bytes.
    fn try_to_bytes(&self) -> io::Result<Vec<u8>> {
        let mut buf = Vec::new();
        self.to_stream(&mut WriteStream::from_writer(&mut buf))?;
        Ok(buf)
    }

    /// Infallible convenience for values already known to fit the wire format.
    /// Dynamic protocol responses must use `try_to_bytes` instead.
    fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        let mut s = WriteStream::from_writer(Cursor::new(&mut buf));
        self.to_stream(&mut s).unwrap();
        buf
    }
}

impl FromStream for bool {
    fn from_stream<R>(stream: &mut ReadStream<R>) -> std::result::Result<Self, FromStreamError>
    where
        R: ReadBytesExt,
        Self: Sized,
    {
        stream.bool()
    }
}

impl ToStream for bool {
    fn to_stream<W>(&self, stream: &mut WriteStream<W>) -> io::Result<usize>
    where
        W: WriteBytesExt,
    {
        stream.bool(*self)
    }

    fn to_bytes(&self) -> Vec<u8> {
        u8::from(*self).to_le_bytes().to_vec()
    }
}

macro_rules! impl_num {
    ($i: ident) => {
        impl FromStream for $i {
            fn from_stream<R>(stream: &mut ReadStream<R>) -> std::result::Result<Self, FromStreamError>
            where
                R: ReadBytesExt,
                Self: Sized,
            {
                stream.$i()
            }
        }

        impl ToStream for $i {
            fn to_stream<W>(&self, stream: &mut WriteStream<W>) -> io::Result<usize>
            where
                W: WriteBytesExt,
            {
                stream.$i(*self)
            }

            fn to_bytes(&self) -> Vec<u8> {
                self.to_le_bytes().to_vec()
            }
        }
    };
}

impl_num!(u8);
impl_num!(u16);
impl_num!(u32);
impl_num!(u64);
impl_num!(i8);
impl_num!(i16);
impl_num!(i32);
impl_num!(i64);
impl_num!(f32);
impl_num!(f64);

impl<const N: usize> FromStream for [u8; N] {
    #[allow(non_snake_case)]
    fn from_stream<R>(stream: &mut ReadStream<R>) -> std::result::Result<Self, FromStreamError>
    where
        R: ReadBytesExt,
        Self: Sized,
    {
        let buf = stream.read_n_bytes(N)?;
        if buf.len() == N {
            let mut tmp = [0u8; N];
            tmp.copy_from_slice(&buf);
            Ok(tmp)
        } else {
            Err(io::Error::new(io::ErrorKind::UnexpectedEof, "").into())
        }
    }
}

impl<const N: usize> ToStream for [u8; N] {
    fn to_stream<W>(&self, stream: &mut WriteStream<W>) -> io::Result<usize>
    where
        W: WriteBytesExt,
    {
        stream.write_n_bytes(self.as_ref())
    }
}

/// Macro to implement `FromStream` and `ToStream` for tuples of various lengths.
/// This allows automatic serialization/deserialization of tuples where each element
/// implements `FromStream` and `ToStream` respectively.
macro_rules! tuple_impls {
    ($($len:tt => ($($n:tt $name:ident)+))+) => {
        $(
            impl<$($name: FromStream),+> FromStream for ($($name,)+) {
                fn from_stream<R>(stream: &mut ReadStream<R>) ->  std::result::Result<Self, FromStreamError>
                where
                    R: ReadBytesExt,
                    Self: Sized,
                {
                    #![allow(non_snake_case)]

                    $(
                        let $name = stream.read()?;
                    )+
                    Ok(($($name,)+))
                }
            }

            impl<$($name: ToStream),+> ToStream for ($($name,)+) {
                fn to_stream<W>(&self, stream: &mut WriteStream<W>) -> io::Result<usize>
                where
                    W: WriteBytesExt,
                {
                    Ok(
                        0
                        $(
                            +stream.write(&self.$n)?
                        )+
                    )
                }
            }
        )+
    }
}

tuple_impls! {
    1  => (0 T0)
    2  => (0 T0 1 T1)
    3  => (0 T0 1 T1 2 T2)
    4  => (0 T0 1 T1 2 T2 3 T3)
    5  => (0 T0 1 T1 2 T2 3 T3 4 T4)
    6  => (0 T0 1 T1 2 T2 3 T3 4 T4 5 T5)
    7  => (0 T0 1 T1 2 T2 3 T3 4 T4 5 T5 6 T6)
    8  => (0 T0 1 T1 2 T2 3 T3 4 T4 5 T5 6 T6 7 T7)
    9  => (0 T0 1 T1 2 T2 3 T3 4 T4 5 T5 6 T6 7 T7 8 T8)
    10 => (0 T0 1 T1 2 T2 3 T3 4 T4 5 T5 6 T6 7 T7 8 T8 9 T9)
    11 => (0 T0 1 T1 2 T2 3 T3 4 T4 5 T5 6 T6 7 T7 8 T8 9 T9 10 T10)
    12 => (0 T0 1 T1 2 T2 3 T3 4 T4 5 T5 6 T6 7 T7 8 T8 9 T9 10 T10 11 T11)
    /*
    13 => (0 T0 1 T1 2 T2 3 T3 4 T4 5 T5 6 T6 7 T7 8 T8 9 T9 10 T10 11 T11 12 T12)
    14 => (0 T0 1 T1 2 T2 3 T3 4 T4 5 T5 6 T6 7 T7 8 T8 9 T9 10 T10 11 T11 12 T12 13 T13)
    15 => (0 T0 1 T1 2 T2 3 T3 4 T4 5 T5 6 T6 7 T7 8 T8 9 T9 10 T10 11 T11 12 T12 13 T13 14 T14)
    16 => (0 T0 1 T1 2 T2 3 T3 4 T4 5 T5 6 T6 7 T7 8 T8 9 T9 10 T10 11 T11 12 T12 13 T13 14 T14 15 T15)
     */
}

impl FromStream for CString {
    /// Reads a `CString` from the stream. It is prefixed by a `u16` length.
    fn from_stream<R>(stream: &mut ReadStream<R>) -> std::result::Result<Self, FromStreamError>
    where
        R: ReadBytesExt,
    {
        let len = stream.u16()?;
        if len == 0 {
            return Ok(CString::default());
        }
        let data = stream.read_n_bytes(len as usize)?;
        Ok(CString::from_vec_with_nul(data)?)
    }
}

impl ToStream for CString {
    /// Writes a `CString` to the stream, prefixed by its `u16` length.
    fn to_stream<W>(&self, stream: &mut WriteStream<W>) -> io::Result<usize>
    where
        W: WriteBytesExt,
    {
        if self.is_empty() {
            return stream.u16(0);
        }
        let data = self.as_bytes_with_nul();
        stream.buf_u16(data)
    }
}

impl FromStream for String {
    /// Reads a `String` from the stream. It is prefixed by a `u16` length and null-terminated.
    fn from_stream<R>(stream: &mut ReadStream<R>) -> std::result::Result<Self, FromStreamError>
    where
        R: ReadBytesExt,
    {
        let cstr = CString::from_stream(stream)?;
        Ok(cstr.into_string().map_err(|e| e.utf8_error())?)
    }
}

impl ToStream for String {
    /// Writes a `String` to the stream, prefixed by its `u16` length and null-terminated.
    fn to_stream<W>(&self, stream: &mut WriteStream<W>) -> io::Result<usize>
    where
        W: WriteBytesExt,
    {
        if self.is_empty() {
            return stream.u16(0);
        }
        let data = self.as_bytes();
        let len = data.len().checked_add(1).and_then(|n| u16::try_from(n).ok())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "string plus terminator exceeds u16"))?;
        Ok(stream.u16(len)? + stream.write_n_bytes(data)? + stream.u8(0)?)
    }
}

impl<T> FromStream for Vec<T>
where
    T: FromStream,
{
    /// Reads a `Vec<T>` from the stream. It is prefixed by a `u32` length, followed by `len` elements of type `T`.
    fn from_stream<R>(stream: &mut ReadStream<R>) -> std::result::Result<Self, FromStreamError>
    where
        R: ReadBytesExt,
    {
        let len = stream.u32()? as usize;
        if len > MAX_COLLECTION_ITEMS {
            return Err(FromStreamError::LengthLimitExceeded(len, MAX_COLLECTION_ITEMS));
        }
        let mut res = Vec::new();
        res.try_reserve_exact(len).map_err(|_| FromStreamError::AllocationFailed(len))?;
        for _ in 0..len {
            res.push(stream.read()?);
        }
        Ok(res)
    }
}

impl<T> ToStream for Vec<T>
where
    T: ToStream,
{
    /// Writes a `Vec<T>` to the stream, prefixed by its `u32` length, followed by its elements.
    fn to_stream<W>(&self, stream: &mut WriteStream<W>) -> io::Result<usize>
    where
        W: WriteBytesExt,
    {
        #[allow(clippy::cast_possible_truncation)]
        let mut total = stream.u32(self.len() as u32)?;
        for item in self {
            total += stream.write(item)?;
        }
        Ok(total)
    }
}

impl<K, V, S: ::std::hash::BuildHasher> ToStream for HashMap<K, V, S>
where
    K: ToStream,
    V: ToStream,
{
    /// Writes a `HashMap<K, V>` to the stream. It is prefixed by a `u32` count,
    /// followed by key-value pairs.
    fn to_stream<W>(&self, stream: &mut WriteStream<W>) -> io::Result<usize>
    where
        W: WriteBytesExt,
    {
        // Format:
        //   count: u32
        //   entries (repeated):
        //     key: K
        //     value: V
        #[allow(clippy::cast_possible_truncation)]
        let mut total = stream.u32(self.len() as u32)?;
        for (k, v) in self {
            total += stream.write(k)?;
            total += stream.write(v)?;
        }
        Ok(total)
    }
}

impl<K, V, S: ::std::hash::BuildHasher + std::default::Default> FromStream for HashMap<K, V, S>
where
    K: FromStream + std::hash::Hash + std::cmp::Eq,
    V: FromStream,
{
    /// Reads a `HashMap<K, V>` from the stream. It is prefixed by a `u32` count,
    /// followed by `count` key-value pairs.
    fn from_stream<R>(stream: &mut ReadStream<R>) -> std::result::Result<Self, FromStreamError>
    where
        R: ReadBytesExt,
        Self: Sized,
    {
        // Format:
        //   count: u32
        //   entries (repeated):
        //     key: K
        //     value: V
        let n = stream.u32()? as usize;
        if n > MAX_COLLECTION_ITEMS {
            return Err(FromStreamError::LengthLimitExceeded(n, MAX_COLLECTION_ITEMS));
        }
        let mut m: Self = HashMap::default();
        m.try_reserve(n).map_err(|_| FromStreamError::AllocationFailed(n))?;
        for _i in 0..n {
            let k = stream.read()?;
            let v = stream.read()?;
            m.insert(k, v);
        }
        Ok(m)
    }
}

#[cfg(test)]
mod allocation_limit_tests {
    use super::*;

    #[test]
    fn oversized_byte_prefix_is_rejected_before_allocation() {
        let bytes = (u32::try_from(MAX_LENGTH_PREFIXED_BYTES).expect("test limit fits u32") + 1).to_le_bytes();
        let mut stream = ReadStream::from_bytes(bytes);
        assert!(matches!(stream.buf_u32(), Err(FromStreamError::LengthLimitExceeded(_, _))));
    }

    #[test]
    fn oversized_vec_count_is_rejected_before_allocation() {
        let bytes = u32::MAX.to_le_bytes();
        assert!(matches!(Vec::<u32>::from_bytes(&bytes), Err(FromStreamError::LengthLimitExceeded(_, _))));
    }

    #[test]
    fn oversized_map_count_is_rejected_before_allocation() {
        let bytes = u32::MAX.to_le_bytes();
        assert!(matches!(HashMap::<u32, u32>::from_bytes(&bytes), Err(FromStreamError::LengthLimitExceeded(_, _))));
    }
}


#[cfg(test)]
mod checked_writer_tests {
    use super::*;

    #[test]
    fn length_prefix_counts_include_prefix_and_valid_bytes_are_unchanged() {
        for n in [0usize,1,255] {
            let data=vec![b'x';n];let mut bytes=Vec::new();
            assert_eq!(WriteStream::from_writer(&mut bytes).buf_u8(&data).unwrap(),n+1);
            assert_eq!(bytes[0],u8::try_from(n).unwrap());assert_eq!(&bytes[1..],&data);
        }
        for n in [0usize,1,65535] {
            let data=vec![b'x';n];let mut bytes=Vec::new();
            assert_eq!(WriteStream::from_writer(&mut bytes).buf_u16(&data).unwrap(),n+2);
            assert_eq!(&bytes[..2],&u16::try_from(n).unwrap().to_le_bytes());assert_eq!(&bytes[2..],&data);
            bytes.clear();assert_eq!(WriteStream::from_writer(&mut bytes).buf_u32(&data).unwrap(),n+4);
            assert_eq!(&bytes[..4],&u32::try_from(n).unwrap().to_le_bytes());assert_eq!(&bytes[4..],&data);
        }
    }
    #[test]
    fn oversized_buffers_are_rejected_before_any_prefix_write() {
        for (n,wide) in [(256usize,false),(65536,true)] {
            let mut bytes=vec![0xaa];let data=vec![0;n];let mut s=WriteStream::from_writer(&mut bytes);
            let result=if wide{s.buf_u16(&data)}else{s.buf_u8(&data)};
            assert_eq!(result.unwrap_err().kind(),io::ErrorKind::InvalidInput);assert_eq!(bytes,[0xaa]);
        }
    }
    #[test]
    fn nul_terminator_counts_toward_string_limit() {
        for n in [0usize,1,65534] {
            let value="x".repeat(n);let bytes=value.try_to_bytes().unwrap();
            assert_eq!(String::from_bytes(&bytes).unwrap(),value);
            assert_eq!(bytes,CString::new(value).unwrap().try_to_bytes().unwrap());
        }
        for n in [65535usize,65536] {
            let value="x".repeat(n);let mut bytes=vec![0xaa];
            assert_eq!(WriteStream::from_writer(&mut bytes).write(&value).unwrap_err().kind(),io::ErrorKind::InvalidInput);
            assert_eq!(bytes,[0xaa]);assert!(value.try_to_bytes().is_err());
            assert!(CString::new(value).unwrap().try_to_bytes().is_err());
        }
    }
    #[test]
    fn utf8_limits_use_bytes_not_characters() {
        let valid="界".repeat(21844);assert_eq!(valid.len(),65532);
        assert_eq!(String::from_bytes(&valid.try_to_bytes().unwrap()).unwrap(),valid);
        assert!("界".repeat(21845).try_to_bytes().is_err());
    }
    #[test]
    fn fallible_encoding_propagates_errors_without_exposing_partial_output() {
        struct Fail;
        impl ToStream for Fail {
            fn to_stream<W:WriteBytesExt>(&self,s:&mut WriteStream<W>)->io::Result<usize>{s.u8(1)?;Err(io::Error::other("fixture failure"))}
        }
        assert_eq!(Fail.try_to_bytes().unwrap_err().kind(),io::ErrorKind::Other);
    }
}
