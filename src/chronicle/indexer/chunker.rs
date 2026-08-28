use super::document::{Chunk, Document};

pub fn chunk(document: &Document, max_length: usize) -> Vec<Chunk> {
    chunk_text(document, max_length)
}

fn chunk_text(document: &Document, max_length: usize) -> Vec<Chunk> {
    if document.content.trim().is_empty() {
        return Vec::new();
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
                chunks.push(std::mem::take(&mut current));
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
            chunks.push(std::mem::take(&mut current));
            paragraph.clone_into(&mut current);
        } else {
            current = candidate;
        }
    }

    if !current.is_empty() {
        chunks.push(current);
    }

    chunks
        .into_iter()
        .enumerate()
        .map(|(index, content)| Chunk {
            document_path: document.path.clone(),
            index,
            content,
            heading: None,
        })
        .collect()
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
                chunks.push(std::mem::take(&mut current));
            }

            word.clone_into(&mut current);
        } else {
            current = candidate;
        }
    }

    if !current.is_empty() {
        chunks.push(current);
    }

    chunks
}
