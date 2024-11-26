package main

import (
	"errors"
	"io/fs"
	"log"
	"os"
	"path/filepath"
	"strconv"
	"strings"

	"github.com/pelletier/go-toml/v2"
)

type Config struct {
	Port              int
	Root              string
	UploadToken       string
	MaxFileSize       int64
	BlacklistedFiles  []string
	AllowedExtensions []string
}

func LoadConfig(path string) (Config, error) {
	cfg := defaultConfig()

	candidates := make([]string, 0, 8)
	if path != "" {
		candidates = append(candidates, path)
	} else {
		candidates = append(candidates, "config.toml")
		if exePath, err := os.Executable(); err == nil {
			exeDir := filepath.Dir(exePath)
			candidates = append(candidates, filepath.Join(exeDir, "config.toml"))
		}
		if xdg := os.Getenv("XDG_CONFIG_HOME"); xdg != "" {
			candidates = append(candidates, filepath.Join(xdg, "serve", "config.toml"))
		}
		if home, err := os.UserHomeDir(); err == nil {
			candidates = append(candidates, filepath.Join(home, ".config", "serve", "config.toml"))
		}
		if home, err := os.UserHomeDir(); err == nil {
			candidates = append(candidates, filepath.Join(home, ".serve", "config.toml"))
		}
	}

	for _, candidate := range candidates {
		if candidate == "" {
			continue
		}
		data, err := os.ReadFile(candidate)
		if err != nil {
			if errors.Is(err, fs.ErrNotExist) {
				continue
			}
			return Config{}, err
		}

		var fileCfg fileConfig
		if err := toml.Unmarshal(data, &fileCfg); err != nil {
			return Config{}, err
		}
		applyFileConfig(&cfg, fileCfg)
		log.Printf("loaded configuration from %s", candidate)
		break
	}

	applyEnvOverrides(&cfg)

	return cfg, nil
}

type fileConfig struct {
	Port              *int     `toml:"port"`
	Root              string   `toml:"root"`
	UploadToken       string   `toml:"upload_token"`
	MaxFileSize       *int64   `toml:"max_file_size"`
	BlacklistedFiles  []string `toml:"blacklisted_files"`
	AllowedExtensions []string `toml:"allowed_extensions"`
}

func applyFileConfig(cfg *Config, fc fileConfig) {
	if fc.Port != nil && *fc.Port > 0 {
		cfg.Port = *fc.Port
	}
	if strings.TrimSpace(fc.Root) != "" {
		cfg.Root = fc.Root
	}
	if strings.TrimSpace(fc.UploadToken) != "" {
		cfg.UploadToken = fc.UploadToken
	}
	if fc.MaxFileSize != nil && *fc.MaxFileSize > 0 {
		cfg.MaxFileSize = *fc.MaxFileSize
	}
	if len(fc.BlacklistedFiles) > 0 {
		cfg.BlacklistedFiles = normalizeList(fc.BlacklistedFiles)
	}
	if len(fc.AllowedExtensions) > 0 {
		cfg.AllowedExtensions = normalizeExtensions(fc.AllowedExtensions)
	}
}

func applyEnvOverrides(cfg *Config) {
	if v, ok := os.LookupEnv("SERVE_PORT"); ok {
		if port, err := strconv.Atoi(v); err == nil && port > 0 {
			cfg.Port = port
		}
	} else if v, ok := os.LookupEnv("ADDR"); ok {
		if port, err := parsePortString(v); err == nil && port > 0 {
			cfg.Port = port
		}
	}

	if v, ok := os.LookupEnv("SERVE_ROOT"); ok {
		if strings.TrimSpace(v) != "" {
			cfg.Root = v
		}
	} else if v, ok := os.LookupEnv("ROOT"); ok {
		if strings.TrimSpace(v) != "" {
			cfg.Root = v
		}
	}

	if v, ok := os.LookupEnv("SERVE_UPLOAD_TOKEN"); ok {
		cfg.UploadToken = v
	} else if v, ok := os.LookupEnv("UPLOAD_TOKEN"); ok {
		cfg.UploadToken = v
	}

	if v, ok := os.LookupEnv("SERVE_MAX_FILE_SIZE"); ok {
		if bytes, err := strconv.ParseInt(v, 10, 64); err == nil && bytes > 0 {
			cfg.MaxFileSize = bytes
		}
	} else if v, ok := os.LookupEnv("MAX_UPLOAD_MB"); ok {
		if mb, err := strconv.ParseInt(v, 10, 64); err == nil && mb > 0 {
			cfg.MaxFileSize = mb << 20
		}
	}

	if v, ok := os.LookupEnv("SERVE_BLACKLIST"); ok {
		list := splitTrim(v, ",")
		if len(list) > 0 {
			cfg.BlacklistedFiles = normalizeList(list)
		}
	} else if v, ok := os.LookupEnv("HIDE"); ok {
		list := splitTrim(v, ",")
		if len(list) > 0 {
			cfg.BlacklistedFiles = normalizeList(list)
		}
	}

	if v, ok := os.LookupEnv("SERVE_ALLOWED_EXT"); ok {
		list := splitTrim(v, ",")
		cfg.AllowedExtensions = normalizeExtensions(list)
	} else if v, ok := os.LookupEnv("ALLOWED_EXT"); ok {
		list := splitTrim(v, ",")
		cfg.AllowedExtensions = normalizeExtensions(list)
	}
}

func defaultConfig() Config {
	return Config{
		Port:              3435,
		Root:              "./share",
		UploadToken:       "abogoboga",
		MaxFileSize:       4000 * 1024 * 1024,
		BlacklistedFiles:  normalizeList(defaultBlacklistedFiles()),
		AllowedExtensions: normalizeExtensions(defaultAllowedExt()),
	}
}

func normalizeList(in []string) []string {
	seen := make(map[string]struct{}, len(in))
	out := make([]string, 0, len(in))
	for _, item := range in {
		trimmed := strings.TrimSpace(item)
		if trimmed == "" {
			continue
		}
		key := strings.ToLower(trimmed)
		if _, ok := seen[key]; ok {
			continue
		}
		seen[key] = struct{}{}
		out = append(out, trimmed)
	}
	return out
}

func normalizeExtensions(in []string) []string {
	seen := make(map[string]struct{}, len(in))
	out := make([]string, 0, len(in))
	for _, item := range in {
		trimmed := strings.TrimSpace(strings.TrimPrefix(item, "."))
		if trimmed == "" {
			continue
		}
		key := strings.ToLower(trimmed)
		if _, ok := seen[key]; ok {
			continue
		}
		seen[key] = struct{}{}
		out = append(out, key)
	}
	return out
}

func defaultAllowedExt() []string {
	return []string{
		"mp3", "wav", "aac", "ogg", "flac", "m4a", "mp4", "avi", "mov", "wmv", "mkv", "flv",
		"webm", "jpg", "jpeg", "png", "gif", "bmp", "tiff", "svg", "zip", "tar", "gz", "bz2", "7z",
		"rar", "exe", "bin", "dll", "deb", "rpm", "iso", "pdf", "doc", "docx", "xls", "xlsx",
		"ppt", "pptx", "txt", "csv", "odt", "rtf", "xml",
	}
}

func defaultBlacklistedFiles() []string {
	return []string{
		".git",
		".DS_Store",
		"__pycache__",
		"_tpl",
		"utils",
		"server.py",
	}
}

func parsePortString(v string) (int, error) {
	v = strings.TrimSpace(v)
	if v == "" {
		return 0, errors.New("empty port")
	}
	idx := strings.LastIndex(v, ":")
	if idx >= 0 {
		v = v[idx+1:]
	}
	port, err := strconv.Atoi(v)
	if err != nil {
		return 0, err
	}
	return port, nil
}
