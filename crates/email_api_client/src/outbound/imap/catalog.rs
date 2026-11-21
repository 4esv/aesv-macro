//! Maps an IMAP folder list onto the mailbox catalog's folders and roles.

use async_imap::types::NameAttribute;

use crate::domain::models::{EmailApiError, FolderRole, MailFolder, ProviderId};

/// One `LIST` entry, with the facts the catalog needs.
#[derive(Debug, Clone)]
pub(super) struct ListedFolder {
    /// The server's mailbox name, used unchanged as the folder's identity.
    pub name: String,
    /// The hierarchy separator, when the server has one.
    pub delimiter: Option<String>,
    /// The RFC 6154 special-use role the server announced.
    pub special_use: Option<FolderRole>,
    /// The folder exists only as a parent and holds no messages.
    pub no_select: bool,
    /// The server said the folder has children.
    pub has_children: bool,
    /// The `STATUS (MESSAGES)` count.
    pub total_items: Option<u32>,
}

impl ListedFolder {
    /// The special-use role among a `LIST` entry's attributes.
    pub fn special_use(attributes: &[NameAttribute<'_>]) -> Option<FolderRole> {
        attributes.iter().find_map(|attribute| match attribute {
            NameAttribute::Sent => Some(FolderRole::Sent),
            NameAttribute::Drafts => Some(FolderRole::Drafts),
            NameAttribute::Trash => Some(FolderRole::Trash),
            NameAttribute::Junk => Some(FolderRole::Junk),
            NameAttribute::Archive => Some(FolderRole::Archive),
            _ => None,
        })
    }
}

/// iCloud's top-level folder names, for a server that does not announce
/// special-use roles. A special-use announcement always wins over a name.
const ROLE_NAMES: [(&str, FolderRole); 7] = [
    ("Sent Messages", FolderRole::Sent),
    ("Sent", FolderRole::Sent),
    ("Drafts", FolderRole::Drafts),
    ("Deleted Messages", FolderRole::Trash),
    ("Trash", FolderRole::Trash),
    ("Junk", FolderRole::Junk),
    ("Archive", FolderRole::Archive),
];

/// Build the catalog. `INBOX` is the inbox in any case (RFC 3501). Each other
/// role goes first to the folder that announces it, then to the first
/// top-level folder with its iCloud name, so a role never lands on two folders.
/// Folders that cannot hold messages are left out, as a stream on one would fail.
pub(super) fn catalog(listed: &[ListedFolder]) -> Result<Vec<MailFolder>, EmailApiError> {
    let announced: Vec<FolderRole> = listed.iter().filter_map(|f| f.special_use).collect();
    let mut named: Vec<FolderRole> = Vec::new();
    let mut folders = Vec::new();

    for folder in listed.iter().filter(|folder| !folder.no_select) {
        let parent = folder
            .delimiter
            .as_deref()
            .and_then(|delimiter| folder.name.rsplit_once(delimiter))
            .map(|(parent, _)| parent);
        let role = if folder.name.eq_ignore_ascii_case("INBOX") {
            FolderRole::Inbox
        } else if let Some(role) = folder.special_use {
            role
        } else {
            let by_name = ROLE_NAMES
                .iter()
                .find(|(name, _)| parent.is_none() && *name == folder.name)
                .map(|(_, role)| *role)
                .filter(|role| !announced.contains(role) && !named.contains(role));
            match by_name {
                Some(role) => {
                    named.push(role);
                    role
                }
                None => FolderRole::Other,
            }
        };

        let display = match parent {
            Some(parent) => &folder.name[parent.len() + 1..],
            None => folder.name.as_str(),
        };
        // A parent that cannot hold messages is not in the catalog, so the
        // child hangs from the root rather than from a folder that is absent.
        let parent_id = parent
            .filter(|parent| {
                listed
                    .iter()
                    .any(|other| other.name == *parent && !other.no_select)
            })
            .map(ProviderId::new)
            .transpose()?;
        let has_children = folder.has_children
            || folder.delimiter.as_deref().is_some_and(|delimiter| {
                let prefix = format!("{}{delimiter}", folder.name);
                listed.iter().any(|other| other.name.starts_with(&prefix))
            });

        folders.push(MailFolder {
            id: ProviderId::new(folder.name.clone())?,
            parent_id,
            // NOTE(shortcut): shows a non-ASCII name in its modified UTF-7 form,
            // decode it (RFC 3501 5.1.3) when a mailbox shows one.
            name: display.to_owned(),
            role,
            has_children,
            total_items: folder.total_items,
        });
    }
    Ok(folders)
}
