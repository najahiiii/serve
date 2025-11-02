const TEMPLATE: &str = include_str!("../templates/template.html");

pub fn render_directory_page(
    directory: &str,
    rows: &str,
    year: i32,
    host: &str,
    disk_usage: &str,
    total_files: usize,
) -> String {
    TEMPLATE
        .replace("{{ directory }}", directory)
        .replace("{{ rows }}", rows)
        .replace("{{ year }}", &year.to_string())
        .replace("{{ host }}", host)
        .replace("{{ disk_usage }}", disk_usage)
        .replace("{{ total_files }}", &total_files.to_string())
}
