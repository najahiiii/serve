const TEMPLATE: &str = include_str!("../templates/template.html");

pub fn render_directory_page(directory: &str, rows: &str, year: i32, host: &str) -> String {
    TEMPLATE
        .replace("{{ directory }}", directory)
        .replace("{{ rows }}", rows)
        .replace("{{ year }}", &year.to_string())
        .replace("{{ host }}", host)
}
