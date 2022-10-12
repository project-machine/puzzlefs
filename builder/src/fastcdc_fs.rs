// It's clear we want to use some kind of content defined chunking algorithm. There are several
// different versions (Rabin, buzhash, FastCDC). The FastCDC paper claims 10x speedup vs. Rabin,
// and per various searches, things like: https://github.com/borgbackup/borg/issues/3026 seem to
// indicate that fastcdc is better buzhash. Let's use FastCDC for now.
//
// Unfortunately (this is not unique to fastcdc), most of these content defined chunking packages
// are written for chunking single files, and generally the streaming API seems to have been added
// as an afterthought. Since we're streaming whole filesystems which are potentially large, the API
// of "here's the chunk offsets" is not really that useful, since we have to 1. re-read the offsets
// to compute the sha256 for generating the blob, 2. want to know *exactly* when a chunk is
// generated so we can stop streaming things to that hash, and 3. want to know the offsets into a
// chunk where a particular buffer ended, because that's going to be our chunk content.
//
// The obvious prior art is casync, which solves this same problem with a completely custom
// implementation of a buzhash. To get started we don't want to do that, so we just do things in
// memory and hope for the best. There is definitely room for improvement here.
//
// The most mature of the rust fastcdc implementations seems to be fastcdc-rs, which we wrap below.
use std::cmp::min;
use std::io;

use fastcdc::FastCDC;

// 'ubuntu' base image is ~40M, as are other base images. If we have any hope of wanting to share
// these, we should allow small chunks.
const MIN_CHUNK_SIZE: usize = 10 * 1024 * 1024;
const AVG_CHUNK_SIZE: usize = 40 * 1024 * 1024;
const MAX_CHUNK_SIZE: usize = 256 * 1024 * 1024;

#[derive(Clone)]
pub struct ChunkWithData {
    pub offset: usize,
    pub length: usize,
    pub data: Box<[u8]>,
}

pub struct FastCDCWrapper {
    min: usize,
    avg: usize,
    max: usize,
    buf: Vec<u8>,
    buf_offset: usize,
    global_offset: usize,
    chunks: Vec<ChunkWithData>,
}

impl FastCDCWrapper {
    pub fn new() -> Self {
        Self::new_with_sizes(MIN_CHUNK_SIZE, AVG_CHUNK_SIZE, MAX_CHUNK_SIZE)
    }

    // we don't expose this since we don't want people to change the algo params, but we do use
    // custom sizes in the tests.
    fn new_with_sizes(min: usize, avg: usize, max: usize) -> Self {
        FastCDCWrapper {
            min,
            avg,
            max,
            buf: vec![0_u8; max],
            buf_offset: 0,
            global_offset: 0,
            chunks: Vec::<ChunkWithData>::new(),
        }
    }

    pub fn get_pending_chunks(&mut self, pending: &mut Vec<ChunkWithData>) {
        pending.clone_from(&self.chunks);
        self.chunks.clear();
    }

    fn render_chunks(&mut self, eof: bool) {
        let chunks = FastCDC::with_eof(
            &self.buf[0..self.buf_offset],
            self.min,
            self.avg,
            self.max,
            eof,
        )
        .collect::<Vec<_>>();
        if chunks.is_empty() {
            return;
        }

        let mut used = 0;
        for chunk in chunks {
            // fix up the offset to be relative to everything that's been written
            let mut data = vec![0; chunk.length].into_boxed_slice();
            data.copy_from_slice(&self.buf[used..used + chunk.length]);
            used += chunk.length;
            self.chunks.push(ChunkWithData {
                offset: self.global_offset + chunk.offset,
                length: chunk.length,
                data,
            })
        }
        self.global_offset += used;

        let leftover = self.buf_offset - used;
        let mut bytes = vec![0_u8; leftover].into_boxed_slice();
        bytes.copy_from_slice(&self.buf[used..used + leftover]);
        self.buf[0..leftover].copy_from_slice(&bytes);
        self.buf_offset = leftover;
    }

    pub fn finish(&mut self) {
        self.render_chunks(true)
    }
}

impl io::Write for FastCDCWrapper {
    fn write(&mut self, write: &[u8]) -> io::Result<usize> {
        let mut write_offset = 0;
        while write_offset != write.len() {
            // copy as much of this write as we can
            let room = min(self.buf.len() - self.buf_offset, write.len() - write_offset);
            let cur = &write[write_offset..write_offset + room];
            if self.buf_offset + cur.len() <= self.buf.len() {
                self.buf[self.buf_offset..self.buf_offset + cur.len()].copy_from_slice(cur);
                self.buf_offset += cur.len();
                write_offset += cur.len();
            }

            // is our buffer full? chunk it
            if self.buf_offset == self.buf.len() {
                self.render_chunks(false);
            }
        }

        Ok(write_offset)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use fastcdc::Chunk;
    use fastrand::Rng;

    use super::*;

    #[test]
    fn test_single_write_ok() {
        // test data stolen from fastcdc-rs, which stole it from the original paper
        let data = fs::read("test/test-1/SekienAkashita.jpg").unwrap();
        let min = 8192;
        let avg = 16384;
        let max = 32768;
        let fcdc_results: Vec<Chunk> = FastCDC::new(&data, min, avg, max).collect();
        let mut wrapper = FastCDCWrapper::new_with_sizes(min, avg, max);

        let mut chunks = Vec::<ChunkWithData>::new();
        io::copy(&mut data.as_slice(), &mut wrapper).unwrap();
        wrapper.finish();
        wrapper.get_pending_chunks(&mut chunks);

        for (i, (fcdc, ours)) in fcdc_results.iter().zip(&chunks).enumerate() {
            assert_eq!(fcdc.offset, ours.offset, "offset {}", i);
            assert_eq!(fcdc.length, ours.length, "length {}", i);
        }
        assert_eq!(fcdc_results.len(), chunks.len(), "number of chunks");
    }

    fn split_buf<T>(mut buf: Vec<T>, chunk_size: usize) -> Vec<Vec<T>> {
        let mut acc = Vec::new();

        while !buf.is_empty() {
            acc.push(buf.drain(0..min(buf.len(), chunk_size)).collect())
        }

        acc
    }

    fn multiple_writes_size(write_size: usize) {
        // test data stolen from fastcdc-rs, which stole it from the original paper
        let data = fs::read("test/test-1/SekienAkashita.jpg").unwrap();
        let min = 8192;
        let avg = 16384;
        let max = 32768;
        let fcdc_results: Vec<Chunk> = FastCDC::new(&data, min, avg, max).collect();
        let mut wrapper = FastCDCWrapper::new_with_sizes(min, avg, max);

        let mut chunks = Vec::<ChunkWithData>::new();
        for write in split_buf(data, write_size) {
            io::copy(&mut write.as_slice(), &mut wrapper).unwrap();
        }

        wrapper.finish();
        wrapper.get_pending_chunks(&mut chunks);

        for (i, (fcdc, ours)) in fcdc_results.iter().zip(&chunks).enumerate() {
            assert_eq!(fcdc.offset, ours.offset, "offset {}", i);
            assert_eq!(fcdc.length, ours.length, "length {}", i);
        }
        assert_eq!(fcdc_results.len(), chunks.len(), "number of chunks");
    }

    #[test]
    fn test_multiple_writes_ok() {
        // writes smaller than the min chunk size
        multiple_writes_size(4096);

        // in between chunk sizes
        multiple_writes_size(8192);

        // bigger than the max chunk size
        multiple_writes_size(40960);

        // giant write (aka, are any of these buffers stack allocated :)
        multiple_writes_size(100 * 1024 * 1024 * 1024);
    }

    #[test]
    fn test_stabilization_rate() {
        const FCDC_MIN: usize = 8192;
        const FCDC_AVG: usize = 16384;
        const FCDC_MAX: usize = 32768;

        fn do_fcdc(seed: u64, modifier: fn(&mut [u8]), chunks: usize) -> Vec<Chunk> {
            let rng = Rng::with_seed(seed);

            let mut buf = vec![0_u8; chunks * FCDC_AVG];
            for x in buf.iter_mut() {
                *x = rng.u8(0..255)
            }
            modifier(&mut buf);
            FastCDC::new(&buf, FCDC_MIN, FCDC_AVG, FCDC_MAX).collect()
        }

        let original = do_fcdc(0, |_| {}, 10);

        fn change_one_byte(buf: &mut [u8]) {
            // change a byte in the "2nd" chunk (well, the average second chunk...)
            buf[2 * FCDC_AVG] ^= 0xff_u8;
        }

        let changed_one = do_fcdc(0, change_one_byte, 10);

        // FCDC is good enough to chunk things exactly the same way with only one byte difference
        assert_eq!(original, changed_one);

        fn change_kb(buf: &mut [u8]) {
            for i in 0..1024 {
                buf[2 * FCDC_AVG + i] ^= 0xff_u8;
            }
        }
        let changed_kb = do_fcdc(0, change_kb, 10);
        assert_eq!(original, changed_kb);

        // and good enough in the face of a contiguous MIN size chunk
        fn change_min(buf: &mut [u8]) {
            for i in 0..FCDC_MIN {
                buf[2 * FCDC_AVG + i] ^= 0xff_u8;
            }
        }
        let changed_min = do_fcdc(0, change_min, 10);
        assert_eq!(original, changed_min);

        fn change_avg(buf: &mut [u8]) {
            for i in 0..FCDC_AVG {
                buf[2 * FCDC_AVG + i] ^= 0xff_u8;
            }
        }

        // but not an AVG size chunk
        let changed_avg = do_fcdc(0, change_avg, 10);
        assert_ne!(original, changed_avg);

        // though it's *pretty* close...
        assert_eq!(original[0], changed_avg[0]);
        assert_eq!(original[1], changed_avg[1]);
        assert_ne!(original[2], changed_avg[2]);
        assert_ne!(original[3], changed_avg[3]);
        assert_eq!(original[4], changed_avg[4]);
        assert_eq!(original[5], changed_avg[5]);
        assert_eq!(original[6], changed_avg[6]);
        assert_eq!(original[7], changed_avg[7]);
        assert_eq!(original[8], changed_avg[8]);
        assert_eq!(original[9], changed_avg[9]);
    }
}
