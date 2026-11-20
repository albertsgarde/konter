//! The repository on disk: finding its files, reading the books' inputs,
//! writing the journals, having hledger check them (`verify`), fetching bank data
//! through Enable Banking, keeping the store of transactions up to date (`store`),
//! and making new books (`init`). The engine (`konter-engine`) does the rest.

pub mod books;
pub mod fetch;
pub mod init;
pub mod load;
pub mod manifest;
pub mod repo;
pub mod store;
