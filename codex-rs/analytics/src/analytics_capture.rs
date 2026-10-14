use crate::events::TrackEventsRequest;
use std::fs::File;
use std::fs::OpenOptions;
use std::io;
use std::io::Write;
use std::path::Path;

pub(crate) const ANALYTICS_EVENTS_CAPTURE_FILE_ENV_VAR: &str =
    "STEVE_ANALYTICS_EVENTS_CAPTURE_FILE";

pub(crate) fn initialize(path: &Path) -> io::Result<()> {
    open_capture_file(path).map(drop)
}

pub(crate) fn append_payload(
    path: &Path,
    product_sku: Option<&str>,
    payload: &TrackEventsRequest,
) -> io::Result<()> {
    let mut value = serde_json::to_value(payload)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    if let Some(product_sku) = product_sku
        && let Some(object) = value.as_object_mut()
    {
        object.insert(
            "product_sku".to_string(),
            serde_json::Value::String(product_sku.to_string()),
        );
    }
    let mut line = serde_json::to_vec(&value)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    line.push(b'\n');

    let mut file = open_capture_file(path)?;
    file.write_all(&line)?;
    file.flush()
}

fn open_capture_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}
