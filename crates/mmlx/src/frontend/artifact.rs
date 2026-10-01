//! Read-only pairs prevent edits from invalidating source associations.

use super::{SourceFile, SourceMap};

/// An owned AST and dialect-specific syntax map tied to a borrowed source.
///
/// Read-only access preserves their association. The source owner must outlive
/// this artifact; use [`Self::into_ast`] to take the AST for editing without a map.
#[derive(Debug)]
pub struct MappedAst<'source, Ast, Syntax> {
    source: SourceFile<'source>,
    ast: Ast,
    syntax: Syntax,
}

impl<'source, Ast, Syntax> MappedAst<'source, Ast, Syntax> {
    pub(crate) fn new(source: SourceFile<'source>, ast: Ast, syntax: Syntax) -> Self {
        Self {
            source,
            ast,
            syntax,
        }
    }

    /// Return the borrowed source snapshot without copying its text.
    pub fn source(&self) -> SourceFile<'source> {
        self.source
    }

    /// Borrow the parsed AST without permitting edits that invalidate its map.
    pub fn ast(&self) -> &Ast {
        &self.ast
    }

    /// Borrow the dialect-specific syntax ranges associated with the AST.
    pub fn syntax(&self) -> &Syntax {
        &self.syntax
    }

    /// Discard the syntax map when taking the AST for editing.
    pub fn into_ast(self) -> Ast {
        self.ast
    }
}

/// An owned output document and source map tied to a borrowed source snapshot.
///
/// Does not retain the input AST or syntax map, so they can be freed after
/// compilation. The source owner must outlive this artifact. Read-only document
/// access keeps output coordinates valid; maps are not embedded in the document.
#[derive(Debug)]
pub struct Compiled<'source, Document, Location> {
    source: SourceFile<'source>,
    document: Document,
    source_map: SourceMap<Location>,
}

impl<'source, Document, Location> Compiled<'source, Document, Location> {
    pub(crate) fn new(
        source: SourceFile<'source>,
        document: Document,
        source_map: SourceMap<Location>,
    ) -> Self {
        Self {
            source,
            document,
            source_map,
        }
    }

    /// Return the original borrowed source without allocating an index or text.
    pub fn source(&self) -> SourceFile<'source> {
        self.source
    }

    /// Borrow the finalized output document without changing mapped coordinates.
    pub fn document(&self) -> &Document {
        &self.document
    }

    /// Borrow the map from dialect-specific output locations to source ranges.
    pub fn source_map(&self) -> &SourceMap<Location> {
        &self.source_map
    }

    /// Discard the map when taking the document for editing or playback.
    pub fn into_document(self) -> Document {
        self.document
    }

    /// Extract the map and document; subsequent edits can invalidate the map.
    pub fn into_parts(self) -> (Document, SourceMap<Location>) {
        (self.document, self.source_map)
    }
}
