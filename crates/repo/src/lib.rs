//! The repository on disk: finding its files, reading the books' inputs,
//! writing the journals, having hledger check them (`verify`), and fetching bank data
//! through Enable Banking. The engine (`konter-engine`) does the rest.

pub mod books;
pub mod fetch;
pub mod load;
pub mod manifest;
pub mod repo;
