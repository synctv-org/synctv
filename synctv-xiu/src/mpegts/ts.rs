use {
    super::{
        define,
        define::{epat_pid, epes_stream_id, ts},
        errors::{MpegTsError, MpegTsErrorValue},
        pat, pes,
        pes::PesMuxer,
        pmt, utils,
    },
    crate::bytesio::{bytes_reader::BytesReader, bytes_writer::BytesWriter},
    bytes::{BufMut, BytesMut},
};

pub struct TsMuxer {
    pub bytes_writer: BytesWriter,
    pat_continuity_counter: u8,
    pmt_continuity_counter: u8,
    h264_h265_with_aud: bool,
    pid: u16,
    pat_period: i64,
    pcr_period: i64,
    pcr_clock: i64,
    pat: pat::Pat,
    cur_pmt_index: usize,
    cur_stream_index: usize,

    packet_number: usize,
}

impl Default for TsMuxer {
    fn default() -> Self {
        Self::new()
    }
}
impl TsMuxer {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            bytes_writer: BytesWriter::new(),
            pat_continuity_counter: 0,
            pmt_continuity_counter: 0,
            h264_h265_with_aud: false,
            pid: 0x0100,
            pat_period: 0,
            pcr_period: 80 * 90,
            pcr_clock: 0,
            pat: pat::Pat::new(),
            cur_pmt_index: 0,
            cur_stream_index: 0,
            packet_number: 0,
        }
    }

    pub const fn reset(&mut self) {
        self.pat_period = 0;
        self.pcr_period = 80 * 90;
        self.pcr_clock = 0;

        self.packet_number = 0;
    }

    pub fn get_data(&mut self) -> BytesMut {
        self.bytes_writer.extract_current_bytes()
    }

    pub fn write(
        &mut self,
        pid: u16,
        pts: i64,
        dts: i64,
        flags: u16,
        payload: BytesMut,
    ) -> Result<(), MpegTsError> {
        self.h264_h265_with_aud = (flags & define::MPEG_FLAG_H264_H265_WITH_AUD) > 0;

        //print!("pes payload length {}\n", payload.len());
        //self.packet_number += payload.len();
        //print!("pes payload sum length {}\n", self.payload_sum);

        self.find_stream(pid)?;

        let cur_pmt = self
            .pat
            .pmt
            .get_mut(self.cur_pmt_index)
            .ok_or(MpegTsError {
                value: MpegTsErrorValue::StreamNotFound,
            })?;
        let cur_stream = cur_pmt
            .streams
            .get_mut(self.cur_stream_index)
            .ok_or(MpegTsError {
                value: MpegTsErrorValue::StreamNotFound,
            })?;

        if 0x1FFF == cur_pmt.pcr_pid
            || (define::epes_stream_id::PES_SID_VIDEO
                == (cur_stream.stream_id & define::epes_stream_id::PES_SID_VIDEO)
                && (cur_pmt.pcr_pid != cur_stream.pid))
        {
            cur_pmt.pcr_pid = cur_stream.pid;
            self.pat_period = 0;
        }

        if cur_pmt.pcr_pid == cur_stream.pid {
            self.pcr_clock += 1;
        }

        cur_stream.pts = pts;
        cur_stream.dts = dts;

        if (flags & define::MPEG_FLAG_IDR_FRAME) > 0 {
            cur_stream.data_alignment_indicator = 1; // idr frame
        } else {
            cur_stream.data_alignment_indicator = 0; // idr frame
        }

        if 0 == self.pat_period || (self.pat_period + define::PAT_PERIOD) <= dts {
            self.pat_period = dts;
            let pat_data = pat::PatMuxer::new().write(self.pat.clone())?;

            self.write_ts_header_for_pat_pmt(
                epat_pid::PAT_TID_PAS,
                pat_data,
                self.pat_continuity_counter,
            )?;
            self.pat_continuity_counter = (self.pat_continuity_counter + 1) % 16;
            self.packet_number += 1;

            for pmt_data in &mut self.pat.pmt.clone() {
                let payload_data = pmt::PmtMuxer::new().write(pmt_data)?;
                self.write_ts_header_for_pat_pmt(
                    pmt_data.pid,
                    payload_data,
                    self.pmt_continuity_counter,
                )?;
                self.pmt_continuity_counter = (self.pmt_continuity_counter + 1) % 16;
                self.packet_number += 1;
            }
        }

        self.write_pes(payload)?;

        Ok(())
    }

    pub fn write_ts_header_for_pat_pmt(
        &mut self,
        pid: u16,
        payload: BytesMut,
        continuity_counter: u8,
    ) -> Result<(), MpegTsError> {
        /*sync byte*/
        self.bytes_writer.write_u8(0x47)?; //0
                                           /*PID 13 bits*/
        self.bytes_writer
            .write_u8(0x40 | ((pid >> 8) as u8 & 0x1F))?; //1

        self.bytes_writer.write_u8(pid as u8)?; //2

        self.bytes_writer.write_u8(0x10 | continuity_counter)?;

        /*adaption field control*/
        self.bytes_writer.write_u8(0x00)?; //4

        /*payload data*/
        self.bytes_writer.write(&payload)?;

        let left_size = ts::TS_PACKET_SIZE
            .saturating_sub(payload.len() as u8)
            .saturating_sub(5);
        for _ in 0..left_size {
            self.bytes_writer.write_u8(0xFF)?;
        }
        Ok(())
    }
    //2.4.3.6 PES packet P35
    pub fn write_pes(&mut self, payload: BytesMut) -> Result<(), MpegTsError> {
        let mut is_start: bool = true;
        let mut payload_reader = BytesReader::new(payload);

        while !payload_reader.is_empty() {
            //write pes header
            let mut pes_muxer = PesMuxer::new();
            if is_start {
                let cur_pmt = self
                    .pat
                    .pmt
                    .get_mut(self.cur_pmt_index)
                    .ok_or(MpegTsError {
                        value: MpegTsErrorValue::StreamNotFound,
                    })?;
                let stream_data =
                    cur_pmt
                        .streams
                        .get_mut(self.cur_stream_index)
                        .ok_or(MpegTsError {
                            value: MpegTsErrorValue::StreamNotFound,
                        })?;
                pes_muxer.write_pes_header(
                    payload_reader.len(),
                    stream_data,
                    self.h264_h265_with_aud,
                )?;
            }

            let pes_header_length: usize = pes_muxer.len();
            let mut payload_length = payload_reader.len();

            //write ts header
            let mut ts_header = BytesWriter::new();
            payload_length = self.write_ts_header_for_pes(
                &mut ts_header,
                pes_header_length,
                payload_length,
                is_start,
            )?;
            self.packet_number += 1;

            is_start = false;

            let data = payload_reader.read_bytes(payload_length)?;

            self.bytes_writer.append(&mut ts_header);
            self.bytes_writer.append(&mut pes_muxer.bytes_writer);
            self.bytes_writer.write(&data[..])?;
        }
        Ok(())
    }
    pub fn write_ts_header_for_pes(
        &mut self,

        ts_header: &mut BytesWriter,
        pes_header_length: usize,
        payload_data_length: usize,
        is_start: bool,
    ) -> Result<usize, MpegTsError> {
        let cur_pmt = self
            .pat
            .pmt
            .get_mut(self.cur_pmt_index)
            .ok_or(MpegTsError {
                value: MpegTsErrorValue::StreamNotFound,
            })?;
        let stream_data = cur_pmt
            .streams
            .get_mut(self.cur_stream_index)
            .ok_or(MpegTsError {
                value: MpegTsErrorValue::StreamNotFound,
            })?;

        let pcr_pid = cur_pmt.pcr_pid;

        /****************************************************************/
        /*        ts header 4 bytes without adaptation filed            */
        /*****************************************************************
         0                   1                   2                   3
         0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
        +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
        |   sync byte   | | | |          PID            |   |   |       |
        +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
        */

        /*sync byte*/
        ts_header.write_u8(0x47)?; //0

        /*PID 13 bits*/
        ts_header.write_u8((stream_data.pid >> 8) as u8 & 0x1F)?; //1
        ts_header.write_u8((stream_data.pid & 0xFF) as u8)?; //2

        /*continuity counter 4 bits*/
        ts_header.write_u8(0x10 | (stream_data.continuity_counter & 0x0F))?; //3
        stream_data.continuity_counter = (stream_data.continuity_counter + 1) % 16;

        if is_start {
            /*payload unit start indicator*/
            ts_header.or_u8_at(1, define::TS_PAYLOAD_UNIT_START_INDICATOR)?;

            if (stream_data.pid == pcr_pid)
                || ((stream_data.data_alignment_indicator > 0)
                    && define::PTS_NO_VALUE != stream_data.pts)
            {
                /*adaption field control*/
                ts_header.or_u8_at(3, 0x20)?;

                /*adaption filed length -- set value to 1 for flags*/
                ts_header.write_u8(0x01)?; //4

                /*will be used for adaptation field flags if have*/
                ts_header.write_u8(0x00)?; //5

                if stream_data.pid == pcr_pid {
                    /*adaption field flags*/
                    ts_header.or_u8_at(5, define::AF_FLAG_PCR)?;

                    let pcr = if define::PTS_NO_VALUE == stream_data.dts {
                        stream_data.pts
                    } else {
                        stream_data.dts
                    };
                    let mut pcr_result: BytesWriter = BytesWriter::new();
                    utils::pcr_write(&mut pcr_result, pcr * 300)?;
                    ts_header.write(&pcr_result.extract_current_bytes()[..])?;
                    /*adaption filed length -- add 6 for pcr length*/
                    ts_header.add_u8_at(4, 6)?;
                }

                if (stream_data.data_alignment_indicator > 0)
                    && define::PTS_NO_VALUE != stream_data.pts
                {
                    /*adaption field flags*/
                    ts_header.or_u8_at(5, define::AF_FLAG_RANDOM_ACCESS_INDICATOR)?;
                }
            }
        }

        /*
        +-------------------------------------------------------------------------+
        |        ts header                              | PES data                |
        +-------------------------------------------------------------------------+
        | 4bytes fixed header | adaption field(stuffing)| pes header | pes payload|
        +-------------------------------------------------------------------------+
        */
        // If payload data cannot fill up the 188 bytes packet,
        // then stuffling bytes need to be filled in the adaptation field,

        let ts_header_length = ts_header.len();
        let mut stuffing_length = define::TS_PACKET_SIZE as i32
            - (ts_header_length + pes_header_length + payload_data_length) as i32;

        if stuffing_length > 0 {
            // Check adaptation field control bit (bit 5 of byte 3)
            // ts_header has at least 4 bytes at this point (sync byte + PID + flags)
            let has_adaptation_field = ts_header.get(3).is_some_and(|b| (b & 0x20) > 0);
            if has_adaptation_field {
                /*adaption filed length -- add 6 for pcr length*/
                ts_header.add_u8_at(4, stuffing_length as u8)?;
            } else {
                /*adaption field control*/
                ts_header.or_u8_at(3, 0x20)?;
                /*AF length，because it occupys one byte,so here sub one.*/
                stuffing_length -= 1;
                /*adaption filed length*/
                ts_header.write_u8(stuffing_length as u8)?;
                /*add flag*/
                if stuffing_length >= 1 {
                    /*adaptation field flags flag occupies one byte, sub one.*/
                    stuffing_length -= 1;
                    ts_header.write_u8(0x00)?;
                }
            }
            for _ in 0..stuffing_length {
                ts_header.write_u8(0xFF)?;
            }
        } else {
            return Ok(define::TS_PACKET_SIZE
                .saturating_sub(ts_header_length)
                .saturating_sub(pes_header_length));
        }

        Ok(payload_data_length)
    }

    pub fn find_stream(&mut self, pid: u16) -> Result<(), MpegTsError> {
        for (pmt_index, pmt) in self.pat.pmt.iter_mut().enumerate() {
            for (stream_index, stream) in pmt.streams.iter_mut().enumerate() {
                if stream.pid == pid {
                    self.cur_pmt_index = pmt_index;
                    self.cur_stream_index = stream_index;

                    return Ok(());
                }
            }
        }

        // for pmt in self.pat.pmt.iter_mut() {
        //     for stream in pmt.streams.iter_mut() {
        //         if stream.pid == pid {
        //             self.cur_pmt_index = pmt_index;
        //             self.cur_stream_index = stream_index;

        //             return Ok(());
        //         }
        //         stream_index += 1;
        //     }
        //     pmt_index += 1;
        // }

        Err(MpegTsError {
            value: MpegTsErrorValue::StreamNotFound,
        })
    }

    pub fn add_stream(&mut self, codecid: u8, extra_data: BytesMut) -> Result<u16, MpegTsError> {
        if self.pat.pmt.is_empty() {
            self.add_program(1, BytesMut::new())?;
        }

        self.pmt_add_stream(0, codecid, extra_data)
    }

    pub fn pmt_add_stream(
        &mut self,
        pmt_index: usize,
        codecid: u8,
        extra_data: BytesMut,
    ) -> Result<u16, MpegTsError> {
        let pmt = &mut self.pat.pmt[pmt_index];

        if pmt.streams.len() == 4 {
            return Err(MpegTsError {
                value: MpegTsErrorValue::StreamCountExeceed,
            });
        }

        let mut cur_stream = pes::Pes::new(); //&mut pmt.streams[pmt.stream_count];

        cur_stream.codec_id = codecid;
        cur_stream.pid = self.pid;
        self.pid += 1;

        if utils::is_steam_type_video(codecid) {
            cur_stream.stream_id = epes_stream_id::PES_SID_VIDEO;
        } else if utils::is_steam_type_audio(codecid) {
            cur_stream.stream_id = epes_stream_id::PES_SID_AUDIO;
        } else {
            cur_stream.stream_id = epes_stream_id::PES_SID_PRIVATE_1;
        }

        if !extra_data.is_empty() {
            cur_stream.esinfo.put(extra_data);
        }

        pmt.streams.push(cur_stream);
        pmt.version_number = (pmt.version_number + 1) % 32;

        self.reset();

        Ok(self.pid.saturating_sub(1))
    }

    pub fn add_program(&mut self, program_number: u16, info: BytesMut) -> Result<(), MpegTsError> {
        for cur_pmt in &self.pat.pmt {
            if cur_pmt.program_number == program_number {
                return Err(MpegTsError {
                    value: MpegTsErrorValue::ProgramNumberExists,
                });
            }
        }

        if self.pat.pmt.len() == 4 {
            return Err(MpegTsError {
                value: MpegTsErrorValue::PmtCountExeceed,
            });
        }
        let mut cur_pmt = pmt::Pmt::new(); //&mut self.pat.pmt[self.pat.pmt_count];

        cur_pmt.pid = self.pid;
        self.pid += 1;
        cur_pmt.program_number = program_number;
        cur_pmt.version_number = 0x00;
        cur_pmt.continuity_counter = 0;
        cur_pmt.pcr_pid = 0x1FFF;

        if !info.is_empty() {
            cur_pmt.program_info.put(&info[..]);
        }

        self.pat.pmt.push(cur_pmt);

        Ok(())
    }
}
