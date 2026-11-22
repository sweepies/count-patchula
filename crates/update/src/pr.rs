//! The pull request lifecycle, split so decisions are pure and requests
//! stay on the I/O side: [`view`] is the reads the lifecycle made and the
//! admission decisions over them, [`body`] is what the bodies say and the
//! writes that produce them, and [`banner`] is the note his section shows
//! while a slow rebuild runs.
pub(crate) mod banner;
pub(crate) mod body;
pub(crate) mod view;
