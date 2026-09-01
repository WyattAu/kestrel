//! vCard (RFC 6350) parser.
//!
//! Parses vCard 3.0/4.0 data into [`Contact`] types.

use crate::types::{Contact, EmailAddr, Phone};

/// Parses a vCard string into a list of contacts.
///
/// # Errors
/// Returns `Err` if the input is completely unparseable.
pub fn parse_vcard(data: &str) -> Result<Vec<Contact>, String> {
    let mut contacts = Vec::new();
    let mut in_contact = false;
    let mut current = Contact::default();

    for line in data.lines() {
        let line = line.trim();
        if line == "BEGIN:VCARD" {
            in_contact = true;
            current = Contact::default();
        } else if line == "END:VCARD" {
            if in_contact {
                contacts.push(current.clone());
            }
            in_contact = false;
        } else if in_contact {
            parse_vcard_property(line, &mut current);
        }
    }

    Ok(contacts)
}

fn parse_vcard_property(line: &str, contact: &mut Contact) {
    let Some((key_params, value)) = line.split_once(':') else {
        return;
    };

    let key = if let Some((k, _params)) = key_params.split_once(';') {
        k
    } else {
        key_params
    };

    match key {
        "UID" => contact.uid = value.to_string(),
        "FN" => contact.display_name = value.to_string(),
        "N" => {
            // N:family;given;additional;prefix;suffix
            let parts: Vec<&str> = value.split(';').collect();
            if !parts.is_empty() && !parts[0].is_empty() {
                contact.family_name = Some(parts[0].to_string());
            }
            if parts.len() > 1 && !parts[1].is_empty() {
                contact.given_name = Some(parts[1].to_string());
            }
        }
        "EMAIL" => {
            contact.email_addresses.push(EmailAddr {
                address: value.to_string(),
                label: None,
            });
        }
        "TEL" => {
            contact.phone_numbers.push(Phone {
                number: value.to_string(),
                label: None,
            });
        }
        "ORG" => contact.organization = Some(value.to_string()),
        "CREATED" => {
            if let Ok(ts) = value.parse::<i64>() {
                contact.created_at = ts;
            }
        }
        "REV" => {
            if let Ok(ts) = value.parse::<i64>() {
                contact.updated_at = ts;
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_vcard() {
        let vcard = "\
BEGIN:VCARD
VERSION:4.0
UID:contact-1@example.com
FN:Jane Doe
N:Doe;Jane;;;
EMAIL:jane@example.com
TEL:+1-555-0100
ORG:Example Corp
CREATED:1700000000
REV:1700100000
END:VCARD";

        let contacts = parse_vcard(vcard).expect("parse should succeed");
        assert_eq!(contacts.len(), 1);

        let contact = &contacts[0];
        assert_eq!(contact.uid, "contact-1@example.com");
        assert_eq!(contact.display_name, "Jane Doe");
        assert_eq!(contact.given_name.as_deref(), Some("Jane"));
        assert_eq!(contact.family_name.as_deref(), Some("Doe"));
        assert_eq!(contact.email_addresses.len(), 1);
        assert_eq!(contact.email_addresses[0].address, "jane@example.com");
        assert_eq!(contact.phone_numbers.len(), 1);
        assert_eq!(contact.phone_numbers[0].number, "+1-555-0100");
        assert_eq!(contact.organization.as_deref(), Some("Example Corp"));
        assert_eq!(contact.created_at, 1_700_000_000);
        assert_eq!(contact.updated_at, 1_700_100_000);
    }

    #[test]
    fn parse_vcard_with_multiple_emails() {
        let vcard = "\
BEGIN:VCARD
VERSION:4.0
UID:multi@example.com
FN:Multi Email
N:Email;Multi;;;
EMAIL:work@example.com
EMAIL:home@example.com
EMAIL:other@example.com
END:VCARD";

        let contacts = parse_vcard(vcard).expect("parse should succeed");
        assert_eq!(contacts.len(), 1);
        assert_eq!(contacts[0].email_addresses.len(), 3);
        assert_eq!(contacts[0].email_addresses[0].address, "work@example.com");
        assert_eq!(contacts[0].email_addresses[1].address, "home@example.com");
        assert_eq!(contacts[0].email_addresses[2].address, "other@example.com");
    }

    #[test]
    fn parse_multiple_vcards() {
        let vcard = "\
BEGIN:VCARD
VERSION:4.0
UID:first@example.com
FN:First Person
N:Person;First;;;
END:VCARD
BEGIN:VCARD
VERSION:4.0
UID:second@example.com
FN:Second Person
N:Person;Second;;;
END:VCARD";

        let contacts = parse_vcard(vcard).expect("parse should succeed");
        assert_eq!(contacts.len(), 2);
        assert_eq!(contacts[0].uid, "first@example.com");
        assert_eq!(contacts[1].uid, "second@example.com");
    }

    #[test]
    fn parse_empty_input() {
        let contacts = parse_vcard("").expect("parse should succeed");
        assert!(contacts.is_empty());
    }

    #[test]
    fn parse_no_vcards() {
        let vcard = "BEGIN:VCARD\nVERSION:4.0\nEND:VCARD";
        let contacts = parse_vcard(vcard).expect("parse should succeed");
        // No UID/FN set, but contact is pushed
        assert_eq!(contacts.len(), 1);
    }

    #[test]
    fn parse_malformed_lines_ignored() {
        let vcard = "\
BEGIN:VCARD
UID:malformed@example.com
NOT_A_REAL_PROPERTY
FN:Still Works
END:VCARD";

        let contacts = parse_vcard(vcard).expect("parse should succeed");
        assert_eq!(contacts.len(), 1);
        assert_eq!(contacts[0].display_name, "Still Works");
    }

    #[test]
    fn parse_unmatched_begin_ignored() {
        let vcard = "BEGIN:VCARD\nUID:unmatched@example.com";
        let contacts = parse_vcard(vcard).expect("parse should succeed");
        // No END:VCARD, so contact is not pushed
        assert!(contacts.is_empty());
    }

    #[test]
    fn parse_empty_name_parts_not_set() {
        let vcard = "\
BEGIN:VCARD
VERSION:4.0
UID:empty@example.com
FN:Empty Name
N;;;;
END:VCARD";

        let contacts = parse_vcard(vcard).expect("parse should succeed");
        assert_eq!(contacts.len(), 1);
        assert!(contacts[0].given_name.is_none());
        assert!(contacts[0].family_name.is_none());
    }
}
