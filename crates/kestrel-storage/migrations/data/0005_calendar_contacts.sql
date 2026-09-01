-- Calendar and contacts schema (data.db, durable per ADR 0009).
-- CalDAV calendars, events, CardDAV address books, and contacts.

CREATE TABLE calendars (
    id           TEXT NOT NULL PRIMARY KEY,
    account_id   TEXT NOT NULL,
    display_name TEXT NOT NULL,
    color        TEXT,
    sync_token   TEXT,
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL
);

CREATE TABLE events (
    id           TEXT NOT NULL PRIMARY KEY,
    calendar_id  TEXT NOT NULL REFERENCES calendars(id) ON DELETE CASCADE,
    account_id   TEXT NOT NULL,
    uid          TEXT NOT NULL,
    summary      TEXT NOT NULL,
    description  TEXT,
    location     TEXT,
    start_time   INTEGER NOT NULL,
    end_time     INTEGER NOT NULL,
    all_day      INTEGER NOT NULL DEFAULT 0,
    recurrence   TEXT,
    ical_data    TEXT,
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL
);

CREATE TABLE address_books (
    id           TEXT NOT NULL PRIMARY KEY,
    account_id   TEXT NOT NULL,
    display_name TEXT NOT NULL,
    sync_token   TEXT,
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL
);

CREATE TABLE contacts (
    id              TEXT NOT NULL PRIMARY KEY,
    address_book_id TEXT NOT NULL REFERENCES address_books(id) ON DELETE CASCADE,
    account_id      TEXT NOT NULL,
    uid             TEXT NOT NULL,
    display_name    TEXT NOT NULL,
    given_name      TEXT,
    family_name     TEXT,
    email_addresses TEXT,
    phone_numbers   TEXT,
    organization    TEXT,
    photo           BLOB,
    vcard_data      TEXT,
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL
);
