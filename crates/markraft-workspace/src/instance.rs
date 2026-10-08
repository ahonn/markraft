use std::path::PathBuf;
pub enum Request {
    Show,
    OpenPaths(Vec<PathBuf>),
}
pub trait RequestSource {
    fn requests(&self) -> Vec<Request>;
}
pub struct NoRequests;
impl RequestSource for NoRequests {
    fn requests(&self) -> Vec<Request> {
        Vec::new()
    }
}
pub type Instance = Box<dyn RequestSource>;
