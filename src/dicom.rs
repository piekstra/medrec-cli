//! DICOM Part 10 headers, DICOMDIR and the synthetic study writer:
//! medical-common's `pk-med-dicom`, under the name the importer has always
//! used. Only what medrec uses is re-exported, so a later pk-med-dicom tag
//! that renames or drops one of these fails to build here instead of
//! changing medrec's API.

pub use pk_med_dicom::{
    is_dicom, is_dicom_file, read_dicomdir, read_header, synthetic, DicomDirEntry, DicomError,
    Header,
};
