use anyhow::{bail, Context, Result};
use serde::Serialize;
use zbus::blocking::Connection;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, Type, Value};

const SERVICE: &str = "org.freedesktop.secrets";
const PATH: &str = "/org/freedesktop/secrets";
const SERVICE_IFACE: &str = "org.freedesktop.Secret.Service";
const INTERNAL_IFACE: &str = "org.gnome.keyring.InternalUnsupportedGuiltRiddenInterface";
const LOGIN: &str = "/org/freedesktop/secrets/collection/login";

/// The Secret Service secret struct: session, parameters, value, content type.
#[derive(Serialize, Type)]
struct Secret<'a> {
    session: ObjectPath<'a>,
    parameters: Vec<u8>,
    value: Vec<u8>,
    content_type: String,
}

/// The interface seahorse uses. The keyring file is never touched directly.
pub fn change_password(old: &str, new: &str) -> Result<()> {
    let connection = Connection::session().context("connecting to the session bus")?;

    let reply = connection
        .call_method(
            Some(SERVICE),
            PATH,
            Some(SERVICE_IFACE),
            "OpenSession",
            &("plain", Value::new("")),
        )
        .context("opening a plain session with gnome-keyring")?;
    let (_output, session): (Value, OwnedObjectPath) = reply
        .body()
        .deserialize()
        .context("reading the session path")?;

    let session = session.as_ref();
    let old = Secret {
        session: session.clone(),
        parameters: Vec::new(),
        value: old.as_bytes().to_vec(),
        content_type: "text/plain".to_string(),
    };
    let new = Secret {
        session: session.clone(),
        parameters: Vec::new(),
        value: new.as_bytes().to_vec(),
        content_type: "text/plain".to_string(),
    };

    let collection = ObjectPath::try_from(LOGIN).context("the login collection path")?;
    let result = connection.call_method(
        Some(SERVICE),
        PATH,
        Some(INTERNAL_IFACE),
        "ChangeWithMasterPassword",
        &(collection, old, new),
    );

    match result {
        Ok(_) => Ok(()),
        Err(e) => bail!("the keyring refused the change: {e}"),
    }
}
