//! A minimal PDF writer for synthetic test documents: Helvetica text on
//! letter-size pages, nothing else. The tests and the dogfood script use it
//! to make documents that carry no real data.

/// A PDF whose single page carries `lines`.
pub fn text_pdf(lines: &[&str]) -> Vec<u8> {
    pages_pdf(&[lines.to_vec()])
}

/// A PDF with one page per entry of `pages`; an empty entry is a page with
/// no text layer.
pub fn pages_pdf(pages: &[Vec<&str>]) -> Vec<u8> {
    let n = pages.len().max(1);
    let mut objects: Vec<String> = Vec::new();
    objects.push("<< /Type /Catalog /Pages 2 0 R >>".into());
    let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", 4 + 2 * i)).collect();
    objects.push(format!(
        "<< /Type /Pages /Kids [{}] /Count {n} >>",
        kids.join(" ")
    ));
    objects.push("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into());
    for i in 0..n {
        let lines = pages.get(i).cloned().unwrap_or_default();
        let mut content = String::new();
        if !lines.is_empty() {
            content.push_str("BT /F1 11 Tf 72 720 Td 15 TL\n");
            for l in &lines {
                content.push_str(&format!("({}) Tj T*\n", escape(l)));
            }
            content.push_str("ET\n");
        }
        objects.push(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents {} 0 R \
             /Resources << /Font << /F1 3 0 R >> >> >>",
            5 + 2 * i
        ));
        objects.push(format!(
            "<< /Length {} >>\nstream\n{content}endstream",
            content.len()
        ));
    }
    let mut out = b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
    );
    for o in offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    out
}

fn escape(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii() && !c.is_ascii_control())
        .map(|c| match c {
            '(' | ')' | '\\' => format!("\\{c}"),
            c => c.to_string(),
        })
        .collect()
}
