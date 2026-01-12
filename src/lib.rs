// Library exports for testing
pub mod backend;
pub mod document;
pub mod parser;
pub mod util;

pub mod analysis {
    pub mod diagnostics;
    pub mod imports;
    pub mod symbols;
    pub mod types;
}

pub mod handlers {
    pub mod completion;
    pub mod definition;
    pub mod formatting;
    pub mod hover;
    pub mod references;
    pub mod rename;
    pub mod symbols;
}
