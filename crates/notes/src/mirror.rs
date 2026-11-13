//! The mise versions mirror's compacted listing: kept whole since a notes
//! read wants every entry the file names.
use patchula_datasource::upstream::MirrorList;

fn compact_mirror(
    body: &mut dyn std::io::Read,
) -> std::result::Result<Vec<u8>, patchula_http::CacheError> {
    let list: MirrorList = patchula_http::conditional::json_document(body)
        .map_err(|_| patchula_http::CacheError::Document)?;
    serde_json::to_vec(&list).map_err(|_| patchula_http::CacheError::Document)
}
pub(crate) const NOTES_MISE_VERSIONS: patchula_http::Derived<patchula_http::CacheError> =
    patchula_http::Derived::new("notes-mise-versions", 1, compact_mirror);
