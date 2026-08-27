use anyhow::Result;

use super::document::{Chunk, Document};

const DEFAULT_MAX_CHUNK_LENGTH: usize = 2_000;

pub fn chunk(document: &Document) -> Result<Vec<Chunk>> {
    chunk_text(document, DEFAULT_MAX_CHUNK_LENGTH)
}

fn chunk_text(document: &Document, max_length: usize) -> Result<Vec<Chunk>> {
    if document.content.trim().is_empty() {
        return Ok(Vec::new());
    }

    let paragraphs = document.content.split("\n\n");

    let mut chunks = Vec::new();
    let mut current = String::new();

    for paragraph in paragraphs {
        let paragraph = paragraph.trim();

        if paragraph.is_empty() {
            continue;
        }

        if paragraph.len() > max_length {
            if !current.is_empty() {
                chunks.push(current);
                current = String::new();
            }

            chunks.extend(split_long_text(paragraph, max_length));
            continue;
        }

        let candidate = if current.is_empty() {
            paragraph.to_owned()
        } else {
            format!("{current}\n\n{paragraph}")
        };

        if candidate.len() > max_length {
            chunks.push(current);
            current = paragraph.to_owned();
        } else {
            current = candidate;
        }
    }

    if !current.is_empty() {
        chunks.push(current);
    }

    Ok(chunks
        .into_iter()
        .enumerate()
        .map(|(index, content)| Chunk {
            document_path: document.path.clone(),
            index,
            content,
            heading: None,
        })
        .collect())
}

fn split_long_text(text: &str, max_length: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();

    for word in text.split_whitespace() {
        let candidate = if current.is_empty() {
            word.to_owned()
        } else {
            format!("{current} {word}")
        };

        if candidate.len() > max_length {
            if !current.is_empty() {
                chunks.push(current);
            }

            current = word.to_owned();
        } else {
            current = candidate;
        }
    }

    if !current.is_empty() {
        chunks.push(current);
    }

    chunks
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

use super::*;

    fn document(content: &str) -> Document {
        Document {
            path: "test.md".into(),
            content: content.to_owned(),
            content_hash: "hash".to_owned(),
        }
    }

    #[test]
    fn empty_document_produces_no_chunks() {
        let chunks = chunk(&document("")).unwrap();

        assert!(chunks.is_empty());
    }

    #[test]
    fn whitespace_only_document_produces_no_chunks() {
        let chunks = chunk(&document("  \n\n  ")).unwrap();

        assert!(chunks.is_empty());
    }

    #[test]
    fn short_document_produces_one_chunk() {
        let chunks = chunk(&document("Hello world.")).unwrap();

        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].index, 0);
        assert_eq!(chunks[0].content, "Hello world.");
        assert_eq!(
            chunks[0].document_path,
            PathBuf::from("test.md")
        );
        assert_eq!(chunks[0].heading, None);
    }

    #[test]
    fn combines_paragraphs_until_limit() {
        let document = document(
            "First paragraph.\n\nSecond paragraph.\n\nThird paragraph.",
        );

        let chunks = chunk_text(&document, 35).unwrap();

        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].index, 0);
        assert_eq!(
            chunks[0].content,
            "First paragraph.\n\nSecond paragraph."
        );
        assert_eq!(chunks[1].index, 1);
        assert_eq!(chunks[1].content, "Third paragraph.");
    }

    #[test]
    fn splits_long_paragraphs() {
        let document =
            document("one two three four five six seven eight");

        let chunks = chunk_text(&document, 15).unwrap();

        assert_eq!(
            chunks
                .iter()
                .map(|chunk| chunk.content.as_str())
                .collect::<Vec<_>>(),
            vec![
                "one two three",
                "four five six",
                "seven eight",
            ]
        );
    }

    #[test]
    fn preserves_document_path() {
        let document = Document {
            path: "people/alice.md".into(),
            content: "Alice.".to_owned(),
            content_hash: "hash".to_owned(),
        };

        let chunks = chunk(&document).unwrap();

        assert_eq!(
            chunks[0].document_path,
            PathBuf::from("people/alice.md")
        );
    }

    #[test]
    fn indexes_chunks_sequentially() {
        let document = document(
            "First.\n\nSecond.\n\nThird.",
        );

        let chunks = chunk_text(&document, 10).unwrap();

        assert_eq!(
            chunks.iter().map(|chunk| chunk.index).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
    }
}