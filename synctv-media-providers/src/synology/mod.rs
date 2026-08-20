mod client;
mod types;

pub use client::SynologyClient;
pub use types::{
    SynologyApiInfo, SynologyApiMap, SynologyAudioTrack, SynologyAudioTrackList, SynologyEpisode,
    SynologyEpisodeList, SynologyFile, SynologyFileList, SynologyHomeVideo, SynologyHomeVideoList,
    SynologyLibrary, SynologyLibraryList, SynologyLogin, SynologyMovie, SynologyMovieList,
    SynologySearchTask, SynologyStreamProfile, SynologyStreamSession, SynologySubtitle,
    SynologyTvRecording, SynologyTvRecordingList, SynologyTvShow, SynologyTvShowList,
    SynologyVideoAdditional, SynologyVideoFile, SynologyVideoItemKind, SynologyVideoMetadata,
};
