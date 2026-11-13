//! PDF inspection (design.md §12.4): detect corrupt files, count pages and
//! check page dimensions. The file itself is passed through unchanged.

use lopdf::{Dictionary, Document, Object};

/// US Letter in PDF points (1/72 inch), the only size providers print.
const LETTER: (f32, f32) = (612.0, 792.0);
const TOLERANCE: f32 = 1.0;

/// The page count of a printable PDF, or why it is not one.
pub fn inspect(bytes: &[u8]) -> Result<u32, String> {
    if !bytes.starts_with(b"%PDF-") {
        return Err("the file is not a PDF".into());
    }
    let document =
        Document::load_mem(bytes).map_err(|_| "the PDF could not be read; it may be damaged")?;
    if document.is_encrypted() {
        return Err("the PDF is password-protected".into());
    }
    let pages = document.get_pages();
    if pages.is_empty() {
        return Err("the PDF has no pages".into());
    }
    for (number, id) in &pages {
        let page = document
            .get_dictionary(*id)
            .map_err(|_| "the PDF could not be read; it may be damaged")?;
        match page_size(&document, page) {
            Some((width, height))
                if (width - LETTER.0).abs() <= TOLERANCE
                    && (height - LETTER.1).abs() <= TOLERANCE => {}
            Some((width, height)) => {
                return Err(format!(
                    "page {number} is {:.2} x {:.2} inches; pages must be US Letter \
                     (8.5 x 11 inches), upright",
                    width / 72.0,
                    height / 72.0
                ));
            }
            None => return Err(format!("page {number} has no readable size")),
        }
    }
    u32::try_from(pages.len()).map_err(|_| "the PDF has too many pages".into())
}

/// A page's size as printed: its media box, which may be inherited from an
/// ancestor, turned by its rotation.
fn page_size(document: &Document, page: &Dictionary) -> Option<(f32, f32)> {
    let media_box = inherited(document, page, b"MediaBox")?.as_array().ok()?;
    let corner: Vec<f32> = media_box
        .iter()
        .filter_map(|n| document.dereference(n).ok()?.1.as_float().ok())
        .collect();
    let [x0, y0, x1, y1] = corner[..] else {
        return None;
    };
    let (width, height) = ((x1 - x0).abs(), (y1 - y0).abs());
    let rotation = inherited(document, page, b"Rotate")
        .and_then(|r| r.as_i64().ok())
        .unwrap_or(0);
    Some(if rotation.rem_euclid(180) == 90 {
        (height, width)
    } else {
        (width, height)
    })
}

/// An attribute of a page, looked up through its ancestors. The depth
/// limit stops a malformed file whose parents form a loop.
fn inherited<'a>(document: &'a Document, page: &'a Dictionary, key: &[u8]) -> Option<&'a Object> {
    let mut node = page;
    for _ in 0..32 {
        if let Ok(value) = node.get_deref(key, document) {
            return Some(value);
        }
        let parent = node.get(b"Parent").ok()?.as_reference().ok()?;
        node = document.get_dictionary(parent).ok()?;
    }
    None
}

#[cfg(test)]
pub(crate) mod tests {
    use lopdf::{Stream, dictionary};

    use super::*;

    pub const LETTER: (i64, i64) = (612, 792);
    pub const A4: (i64, i64) = (595, 842);

    /// A minimal PDF of `pages` blank pages. The size is set once on the
    /// page tree, so every page inherits it.
    pub fn sample(pages: u32, (width, height): (i64, i64)) -> Vec<u8> {
        let mut document = Document::with_version("1.5");
        let tree = document.new_object_id();
        let kids: Vec<Object> = (0..pages)
            .map(|_| {
                let content = document.add_object(Stream::new(dictionary! {}, Vec::new()));
                let page =
                    dictionary! { "Type" => "Page", "Parent" => tree, "Contents" => content };
                document.add_object(page).into()
            })
            .collect();
        let tree_node = dictionary! {
            "Type" => "Pages",
            "Kids" => kids,
            "Count" => i64::from(pages),
            "MediaBox" => vec![0.into(), 0.into(), width.into(), height.into()],
        };
        document.objects.insert(tree, Object::Dictionary(tree_node));
        let catalog = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => tree });
        document.trailer.set("Root", catalog);
        let mut bytes = Vec::new();
        document.save_to(&mut bytes).unwrap();
        bytes
    }

    #[test]
    fn counts_pages_with_inherited_size() {
        assert_eq!(inspect(&sample(1, LETTER)), Ok(1));
        assert_eq!(inspect(&sample(7, LETTER)), Ok(7));
    }

    #[test]
    fn rejects_other_sizes_and_sideways_pages() {
        assert!(inspect(&sample(1, A4)).unwrap_err().contains("US Letter"));
        // Landscape Letter is the wrong way up for a letter...
        assert!(inspect(&sample(1, (792, 612))).is_err());
        // ...unless the page is rotated to print upright.
        let mut document = Document::load_mem(&sample(1, (792, 612))).unwrap();
        let page = *document.get_pages().values().next().unwrap();
        document.get_dictionary_mut(page).unwrap().set("Rotate", 90);
        let mut rotated = Vec::new();
        document.save_to(&mut rotated).unwrap();
        assert_eq!(inspect(&rotated), Ok(1));
    }

    #[test]
    fn rejects_non_pdf_and_damaged_files() {
        assert!(inspect(b"hello").unwrap_err().contains("not a PDF"));
        assert!(
            inspect(b"%PDF-1.7\ngarbage")
                .unwrap_err()
                .contains("damaged")
        );
        assert!(inspect(b"").is_err());
    }
}
