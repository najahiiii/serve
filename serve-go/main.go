package main

import (
	"compress/gzip"
	"embed"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"html/template"
	"io"
	"log"
	"math"
	"mime"
	"mime/multipart"
	"net"
	"net/http"
	"os"
	"path"
	"path/filepath"
	"slices"
	"strings"
	"time"
)

const version = "0.1.0"
const poweredBy = "serve-go/" + version

//go:embed _tpl/index.html
var tplFS embed.FS

var (
	root        string
	hideList    []string
	uploadToken string
	maxUpBytes  int64
	allowedExt  map[string]struct{}
)

var errTooLarge = errors.New("file too large")

func main() {
	args := os.Args[1:]
	if len(args) == 0 {
		printUsage()
		os.Exit(1)
	}

	cmd := args[0]
	switch cmd {
	case "run":
		if err := runCommand(args[1:]); err != nil {
			log.Fatal(err)
		}
	case "init-config":
		if err := initConfig(); err != nil {
			log.Fatal(err)
		}
	case "help", "--help", "-h":
		printUsage()
	case "--version", "-v":
		fmt.Printf("serve-go %s\n", version)
	default:
		if strings.HasPrefix(cmd, "-") {
			if err := runCommand(args); err != nil {
				log.Fatal(err)
			}
			return
		}
		printUsage()
		os.Exit(1)
	}
}

func runCommand(args []string) error {
	fs := flag.NewFlagSet("run", flag.ExitOnError)
	configPath := fs.String("config", "", "Path to configuration file (TOML)")
	portOverride := fs.Int("port", 0, "Override listening port")
	rootOverride := fs.String("root", "", "Override root directory")
	uploadTokenOverride := fs.String("upload-token", "", "Override upload token")
	maxFileSizeOverride := fs.Int64("max-file-size", 0, "Override max upload size in bytes")
	allowedExtOverride := fs.String("allowed-ext", "", "Comma-separated allowed extensions")
	hideOverride := fs.String("hide", "", "Comma-separated hidden files/directories")

	if err := fs.Parse(args); err != nil {
		return err
	}

	cfg, err := LoadConfig(*configPath)
	if err != nil {
		return fmt.Errorf("load config: %w", err)
	}

	if *portOverride > 0 {
		cfg.Port = *portOverride
	}
	if strings.TrimSpace(*rootOverride) != "" {
		cfg.Root = *rootOverride
	}
	if strings.TrimSpace(*uploadTokenOverride) != "" {
		cfg.UploadToken = *uploadTokenOverride
	}
	if *maxFileSizeOverride > 0 {
		cfg.MaxFileSize = *maxFileSizeOverride
	}
	if strings.TrimSpace(*allowedExtOverride) != "" {
		cfg.AllowedExtensions = normalizeExtensions(splitTrim(*allowedExtOverride, ","))
	}
	if strings.TrimSpace(*hideOverride) != "" {
		cfg.BlacklistedFiles = normalizeList(splitTrim(*hideOverride, ","))
	}

	return runServer(cfg)
}

func runServer(cfg Config) error {
	absRoot, err := filepath.Abs(cfg.Root)
	if err != nil {
		return fmt.Errorf("resolve root: %w", err)
	}

	root = absRoot
	hideList = cfg.BlacklistedFiles
	uploadToken = cfg.UploadToken
	maxUpBytes = cfg.MaxFileSize
	allowedExt = loadAllowedExt(cfg.AllowedExtensions)

	addr := fmt.Sprintf(":%d", cfg.Port)

	log.Printf("Config loaded: port=%d token_set=%t max_file_size=%d allowed_ext=%d hidden=%d", cfg.Port, cfg.UploadToken != "", cfg.MaxFileSize, len(cfg.AllowedExtensions), len(cfg.BlacklistedFiles))
	log.Printf("Starting server on %s serving %s", addr, root)

	mux := http.NewServeMux()
	mux.HandleFunc("/", withCommonHeaders(indexOrFile))
	mux.HandleFunc("/upload", withCommonHeaders(upload))

	server := &http.Server{
		Addr:              addr,
		Handler:           mux,
		ReadTimeout:       15 * time.Second,
		ReadHeaderTimeout: 15 * time.Second,
		WriteTimeout:      0,
		IdleTimeout:       120 * time.Second,
		MaxHeaderBytes:    1 << 20,
	}

	return server.ListenAndServe()
}

func printUsage() {
	fmt.Println(`Usage: serve-go <command> [options]

Options:
	--config <file>        Path to TOML configuration file
	--port <n>             Override listening port (default 3435)
	--root <path>          Override root directory to serve
	--upload-token <val>   Override upload token
	--max-file-size <n>    Override max upload size in bytes
	--allowed-ext <list>   Comma-separated list of allowed extensions
	--hide <list>          Comma-separated list of hidden files/directories
	-v, --version          Print version information

Commands:
	run               Start the HTTP file server
	init-config       Generate default config at $HOME/.config/serve/config.toml`)
}

func withCommonHeaders(h http.HandlerFunc) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		// Basic hardening / cache defaults
		w.Header().Set("X-Content-Type-Options", "nosniff")
		w.Header().Set("Accept-Ranges", "bytes")
		w.Header().Set("X-Powered-By", poweredBy)
		if r.Method == http.MethodGet {
			w.Header().Set("Cache-Control", "public, max-age=60")
		}
		h(w, r)
	}
}

func indexOrFile(w http.ResponseWriter, r *http.Request) {
	rel := strings.TrimPrefix(r.URL.Path, "/")
	full := filepath.Join(root, filepath.FromSlash(rel))
	fullAbs, err := filepath.Abs(full)
	if err != nil || !withinRoot(fullAbs) {
		http.Error(w, "forbidden", http.StatusForbidden)
		return
	}

	fi, err := os.Stat(fullAbs)
	if err != nil {
		http.NotFound(w, r)
		return
	}

	if fi.IsDir() {
		// auto-redirect /dir -> /dir/
		// Yoi.
		if !strings.HasSuffix(r.URL.Path, "/") {
			http.Redirect(w, r, r.URL.Path+"/", http.StatusMovedPermanently)
			return
		}
		listDir(w, r, fullAbs, rel)
		return
	}

	if !isInlineView(r) {
		w.Header().Set("Content-Disposition", fmt.Sprintf("attachment; filename=\"%s\"", filepath.Base(fullAbs)))
	}
	pathLog := r.URL.Path
	if pathLog == "" {
		pathLog = "/"
	}
	log.Printf("[downloading] %s - %s - %s - %s", clientIP(r), filepath.Base(fullAbs), pathLog, userAgent(r))
	http.ServeFile(w, r, fullAbs)
}

type entry struct {
	Index       int
	Name        string
	DisplayName string
	SizeHuman   string
	SizeBytes   int64
	ModHuman    string
	IsDir       bool
	URL         string
	Mime        string
}

var dirT = template.Must(template.ParseFS(tplFS, "_tpl/index.html"))

func listDir(w http.ResponseWriter, r *http.Request, absPath, rel string) {
	f, err := os.Open(absPath)
	if err != nil {
		http.Error(w, "cannot open dir", http.StatusInternalServerError)
		return
	}
	defer f.Close()

	ents, err := f.Readdir(0)
	if err != nil {
		http.Error(w, "cannot read dir", http.StatusInternalServerError)
		return
	}

	list := make([]entry, 0, len(ents))
	for _, e := range ents {
		name := e.Name()
		if shouldHide(name) {
			continue
		}
		isDir := e.IsDir()
		urlPath := name
		displayName := name
		if isDir {
			urlPath += "/"
			displayName += "/"
		}
		sizeBytes := int64(0)
		sizeHuman := "-"
		mimeType := "inode/directory"
		if !isDir {
			sizeBytes = e.Size()
			sizeHuman = formatBytes(sizeBytes)
			mimeType = mime.TypeByExtension(strings.ToLower(filepath.Ext(name)))
			if mimeType == "" {
				mimeType = "application/octet-stream"
			}
		}
		modHuman := e.ModTime().Local().Format("2006-01-02 15:04:05")
		list = append(list, entry{
			Name:        name,
			DisplayName: displayName,
			SizeHuman:   sizeHuman,
			SizeBytes:   sizeBytes,
			ModHuman:    modHuman,
			IsDir:       isDir,
			URL:         urlPath,
			Mime:        mimeType,
		})
	}

	slices.SortFunc(list, func(a, b entry) int {
		if a.IsDir != b.IsDir {
			if a.IsDir {
				return -1
			}
			return 1
		}
		return strings.Compare(strings.ToLower(a.Name), strings.ToLower(b.Name))
	})

	for i := range list {
		list[i].Index = i + 1
	}

	if strings.EqualFold(r.Header.Get("X-Serve-Client"), "serve-cli") {
		scheme := schemeFromRequest(r)
		base := fmt.Sprintf("%s://%s", scheme, r.Host)
		relPath := strings.TrimPrefix(rel, "/")
		jsonPath := "/"
		if relPath != "" {
			jsonPath = "/" + relPath
			if !strings.HasSuffix(jsonPath, "/") {
				jsonPath += "/"
			}
		}

		entriesJSON := make([]map[string]any, 0, len(list))
		for _, item := range list {
			relURL := buildItemURL(rel, item.Name, item.IsDir)
			absolute := base + relURL
			entriesJSON = append(entriesJSON, map[string]any{
				"name":       item.Name,
				"size":       item.SizeHuman,
				"size_bytes": item.SizeBytes,
				"modified":   item.ModHuman,
				"url":        absolute,
				"is_dir":     item.IsDir,
				"mime_type":  item.Mime,
			})
		}

		resp := map[string]any{
			"path":       jsonPath,
			"entries":    entriesJSON,
			"powered_by": poweredBy,
		}

		w.Header().Set("Content-Type", "application/json")
		w.Header().Set("X-Powered-By", poweredBy)
		if err := json.NewEncoder(w).Encode(resp); err != nil {
			http.Error(w, "failed to encode json", http.StatusInternalServerError)
		}
		return
	}

	trimmed := strings.TrimSuffix(rel, "/")
	directory := r.Host
	if trimmed != "" {
		directory = trimmed + "/"
	}

	var parentURL string
	if trimmed != "" {
		parent := path.Dir("/" + trimmed)
		if parent == "." || parent == "/" {
			parentURL = "/"
		} else {
			parentURL = parent + "/"
		}
	}

	data := struct {
		Directory string
		Items     []entry
		ParentURL string
		Year      int
		Host      string
	}{
		Directory: directory,
		Items:     list,
		ParentURL: parentURL,
		Year:      time.Now().Year(),
		Host:      r.Host,
	}

	w.Header().Set("Content-Type", "text/html; charset=utf-8")
	writer := io.Writer(w)
	var gz *gzip.Writer
	if acceptsGzip(r.Header) {
		w.Header().Set("Content-Encoding", "gzip")
		w.Header().Add("Vary", "Accept-Encoding")
		gz = gzip.NewWriter(w)
		writer = gz
		defer gz.Close()
	}

	if err := dirT.Execute(writer, data); err != nil {
		http.Error(w, "tpl error", http.StatusInternalServerError)
	}
}

func upload(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodPost {
		w.WriteHeader(http.StatusMethodNotAllowed)
		return
	}
	if uploadToken == "" || r.Header.Get("X-Upload-Token") != uploadToken {
		http.Error(w, "unauthorized", http.StatusUnauthorized)
		return
	}
	limit := maxUpBytes
	if limit <= 0 {
		limit = math.MaxInt64
	}
	r.Body = http.MaxBytesReader(w, r.Body, limit)

	ct := r.Header.Get("Content-Type")
	if !strings.HasPrefix(ct, "multipart/form-data") {
		http.Error(w, "multipart/form-data required", http.StatusBadRequest)
		return
	}
	if err := r.ParseMultipartForm(limit); err != nil {
		http.Error(w, "parse form: "+err.Error(), http.StatusBadRequest)
		return
	}

	targetDir := root
	if headerPath := strings.TrimSpace(r.Header.Get("X-Upload-Path")); headerPath != "" {
		resolved, err := resolveWithinRoot(headerPath)
		if err != nil {
			http.Error(w, "invalid directory path", http.StatusBadRequest)
			return
		}
		targetDir = resolved
	} else if dirParam := strings.TrimSpace(r.FormValue("path")); dirParam != "" {
		resolved, err := resolveWithinRoot(dirParam)
		if err != nil {
			http.Error(w, "invalid directory path", http.StatusBadRequest)
			return
		}
		targetDir = resolved
	}

	file, hdr, err := r.FormFile("file")
	if err != nil {
		http.Error(w, "missing file: "+err.Error(), http.StatusBadRequest)
		return
	}
	defer file.Close()

	dstName := sanitizeName(hdr.Filename)
	if dstName == "" {
		http.Error(w, "bad filename", http.StatusBadRequest)
		return
	}

	allowNoExt := allowNoExtension(r.Header.Get("X-Allow-No-Ext"))
	if ext := strings.ToLower(strings.TrimPrefix(filepath.Ext(dstName), ".")); ext != "" {
		if len(allowedExt) > 0 {
			if _, ok := allowedExt[ext]; !ok {
				http.Error(w, "file type not allowed", http.StatusBadRequest)
				return
			}
		}
		if hdr.Header.Get("Content-Type") == "" {
			hdr.Header.Set("Content-Type", mimeTypeFromExt(ext))
		}
	} else if !allowNoExt {
		http.Error(w, "file type not allowed", http.StatusBadRequest)
		return
	}

	if err := os.MkdirAll(targetDir, 0o755); err != nil {
		http.Error(w, "cannot create directory: "+err.Error(), http.StatusInternalServerError)
		return
	}

	dstPath := filepath.Join(targetDir, dstName)
	dstAbs, err := filepath.Abs(dstPath)
	if err != nil || !withinRoot(dstAbs) {
		http.Error(w, "forbidden", http.StatusForbidden)
		return
	}
	out, err := os.Create(dstAbs)
	if err != nil {
		http.Error(w, "cannot create: "+err.Error(), http.StatusInternalServerError)
		return
	}
	defer out.Close()

	written, err := copyStream(out, file, limit)
	if err != nil {
		_ = os.Remove(dstAbs)
		if errors.Is(err, errTooLarge) {
			http.Error(w, err.Error(), http.StatusRequestEntityTooLarge)
			return
		}
		http.Error(w, "upload failed: "+err.Error(), http.StatusInternalServerError)
		return
	}

	relPath, err := filepath.Rel(root, dstAbs)
	if err != nil {
		relPath = dstName
	}
	relPath = filepath.ToSlash(relPath)

	scheme := schemeFromRequest(r)
	baseURL := fmt.Sprintf("%s://%s/", scheme, r.Host)

	resp := map[string]any{
		"status":       "success",
		"name":         dstName,
		"size":         written,
		"created_date": time.Now().UTC().Format(time.RFC3339),
		"mime_type":    hdr.Header.Get("Content-Type"),
		"path":         relPath,
		"view":         baseURL + relPath + "?view=true",
		"download":     baseURL + relPath,
		"powered_by":   poweredBy,
	}

	log.Printf("[uploading] %s - %s - %s - %s", clientIP(r), dstName, relPath, userAgent(r))

	w.Header().Set("Content-Type", "application/json")
	w.Header().Set("X-Upload-Server", poweredBy)
	if err := json.NewEncoder(w).Encode(resp); err != nil {
		http.Error(w, "encode response: "+err.Error(), http.StatusInternalServerError)
	}
}

func copyStream(dst io.Writer, src multipart.File, limit int64) (int64, error) {
	reader := io.LimitReader(src, limit+1)
	written, err := io.Copy(dst, reader)
	if err != nil {
		return written, err
	}
	if written > limit {
		return limit, errTooLarge
	}
	return written, nil
}

func acceptsGzip(h http.Header) bool {
	return strings.Contains(h.Get("Accept-Encoding"), "gzip")
}

func clientIP(r *http.Request) string {
	if forwarded := r.Header.Get("X-Forwarded-For"); forwarded != "" {
		if idx := strings.Index(forwarded, ","); idx != -1 {
			return strings.TrimSpace(forwarded[:idx])
		}
		return strings.TrimSpace(forwarded)
	}
	if cf := r.Header.Get("CF-Connecting-IP"); cf != "" {
		return cf
	}
	if real := r.Header.Get("X-Real-IP"); real != "" {
		return real
	}
	host, _, err := net.SplitHostPort(r.RemoteAddr)
	if err == nil {
		return host
	}
	return r.RemoteAddr
}

func userAgent(r *http.Request) string {
	ua := strings.TrimSpace(r.UserAgent())
	if ua == "" {
		return "unknown"
	}
	return ua
}

func buildItemURL(current, name string, isDir bool) string {
	trimmed := strings.TrimPrefix(current, "/")
	joined := path.Join(trimmed, name)
	joined = strings.TrimPrefix(joined, "/")
	if isDir {
		if joined != "" && !strings.HasSuffix(joined, "/") {
			joined += "/"
		}
	}
	return "/" + strings.TrimPrefix(joined, "/")
}

func formatBytes(size int64) string {
	if size == 0 {
		return "0 B"
	}
	units := []string{"B", "KB", "MB", "GB", "TB", "PB"}
	value := float64(size)
	idx := 0
	for value >= 1024 && idx < len(units)-1 {
		value /= 1024
		idx++
	}
	return fmt.Sprintf("%.2f %s", value, units[idx])
}

func resolveWithinRoot(rel string) (string, error) {
	joined := filepath.Join(root, filepath.FromSlash(rel))
	abs, err := filepath.Abs(joined)
	if err != nil {
		return "", err
	}
	if !withinRoot(abs) {
		return "", errors.New("outside root")
	}
	return abs, nil
}

func withinRoot(p string) bool {
	cleanRoot := filepath.Clean(root)
	cleanPath := filepath.Clean(p)
	if cleanPath == cleanRoot {
		return true
	}
	prefix := cleanRoot + string(os.PathSeparator)
	return strings.HasPrefix(cleanPath, prefix)
}

func allowNoExtension(v string) bool {
	switch strings.ToLower(strings.TrimSpace(v)) {
	case "1", "true", "yes", "allow":
		return true
	default:
		return false
	}
}

func schemeFromRequest(r *http.Request) string {
	if proto := r.Header.Get("X-Forwarded-Proto"); proto != "" {
		return proto
	}
	if r.TLS != nil {
		return "https"
	}
	return "http"
}

func isInlineView(r *http.Request) bool {
	v := strings.ToLower(strings.TrimSpace(r.URL.Query().Get("view")))
	switch v {
	case "1", "true", "yes", "inline":
		return true
	default:
		return false
	}
}

func loadAllowedExt(list []string) map[string]struct{} {
	if len(list) == 0 {
		return nil
	}
	set := make(map[string]struct{}, len(list))
	for _, item := range list {
		trimmed := strings.TrimSpace(strings.TrimPrefix(item, "."))
		if trimmed == "" {
			continue
		}
		set[strings.ToLower(trimmed)] = struct{}{}
	}
	if len(set) == 0 {
		return nil
	}
	return set
}

func mimeTypeFromExt(ext string) string {
	if ext == "" {
		return "application/octet-stream"
	}
	if !strings.HasPrefix(ext, ".") {
		ext = "." + ext
	}
	if mt := mime.TypeByExtension(ext); mt != "" {
		return mt
	}
	return "application/octet-stream"
}

func shouldHide(name string) bool {
	if strings.HasPrefix(name, ".") {
		return true
	}
	for _, h := range hideList {
		if h != "" && strings.EqualFold(name, h) {
			return true
		}
	}
	return false
}

func sanitizeName(n string) string {
	n = filepath.Base(n)
	n = strings.ReplaceAll(n, string(filepath.Separator), "_")
	n = strings.TrimSpace(n)
	return n
}

func splitTrim(s, sep string) []string {
	if s == "" {
		return nil
	}
	parts := strings.Split(s, sep)
	out := make([]string, 0, len(parts))
	for _, p := range parts {
		t := strings.TrimSpace(p)
		if t != "" {
			out = append(out, t)
		}
	}
	return out
}

func initConfig() error {
	home, err := os.UserHomeDir()
	if err != nil {
		return fmt.Errorf("resolve home: %w", err)
	}
	dir := filepath.Join(home, ".config", "serve")
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return fmt.Errorf("create directory: %w", err)
	}
	path := filepath.Join(dir, "config.toml")
	if _, err := os.Stat(path); err == nil {
		return fmt.Errorf("config already exists at %s", path)
	}
	content := []byte(`# Generated by serve-go
port = 3435
root = "./share"
upload_token = "abogoboga"
max_file_size = 4194304000
blacklisted_files = ["utils", "server.py", "_tpl", ".git"]
allowed_extensions = ["mp3", "wav", "mp4", "zip", "pdf", "png", "jpg"]
`)
	if err := os.WriteFile(path, content, 0o644); err != nil {
		return fmt.Errorf("write config: %w", err)
	}
	fmt.Printf("Config written to %s\n", path)
	return nil
}
