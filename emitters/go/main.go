// SPDX-License-Identifier: Apache-2.0

// tungsten-emit-go generates a Go SDK from the tungsten IR. It is an
// external emitter (protocol 1, documented with tungsten_emit::external):
// tungsten starts it with no arguments, writes one JSON request (the IR and
// the target's options) to standard input and reads one JSON response (the
// files and diagnostics) from standard output. With --describe it prints
// who it is.
//
// The generated package runs on the Go runtime,
// github.com/pboachie/tungsten/runtimes/go.
package main

import (
	"bufio"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"regexp"
	"sort"
	"strings"
)

const (
	protocol       = 1
	emitterVersion = "0.1.0"
)

type diagnostic struct {
	Code     string `json:"code"`
	Severity string `json:"severity"`
	Message  string `json:"message"`
	Help     string `json:"help,omitempty"`
}

type options struct {
	module         string
	pkg            string
	version        string
	runtimeVersion string
	runtimePath    string
}

func describe() any {
	str := func(desc string) map[string]any { return map[string]any{"type": "string", "description": desc} }
	return map[string]any{
		"protocol": protocol,
		"name":     "go",
		"version":  emitterVersion,
		"options": map[string]any{
			"type": "object",
			"properties": map[string]any{
				"module":          str("Go module path of the SDK (default example.com/<api>-sdk-go)."),
				"package":         str("Go package name (default: the API name, lower case)."),
				"version":         str("SDK version sent in User-Agent (default 0.1.0)."),
				"runtime_version": str("Version of github.com/pboachie/tungsten/runtimes/go to require (default v0.1.0)."),
				"runtime_path":    str("Directory of the runtime, relative to the output directory, for a replace directive (local builds)."),
			},
			"additionalProperties": false,
		},
	}
}

var (
	semver      = regexp.MustCompile(`^[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$`)
	modulePath  = regexp.MustCompile(`^[A-Za-z0-9._~-]+(?:/[A-Za-z0-9._~-]+)*$`)
	packageText = regexp.MustCompile(`^[a-z][a-z0-9]*$`)
)

func parseOptions(raw map[string]json.RawMessage, ir *IR) (options, []diagnostic) {
	name := ir.API.Name.Wire
	opts := options{
		module:         "example.com/" + strings.ToLower(strings.Join(ir.API.Name.Words, "-")) + "-sdk-go",
		pkg:            packageName(name),
		version:        "0.1.0",
		runtimeVersion: "v0.1.0",
	}
	var diags []diagnostic
	bad := func(key, want string) {
		diags = append(diags, diagnostic{Code: "GO004", Severity: "error",
			Message: fmt.Sprintf("option `%s` must be %s", key, want), Help: "Fix the go target in tungsten.yml."})
	}
	keys := make([]string, 0, len(raw))
	for k := range raw {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	for _, key := range keys {
		var text string
		if json.Unmarshal(raw[key], &text) != nil {
			bad(key, "a string")
			continue
		}
		switch key {
		case "module":
			if !modulePath.MatchString(text) {
				bad(key, "a Go module path such as example.com/acme-sdk")
				continue
			}
			opts.module = text
		case "package":
			if !packageText.MatchString(text) || goKeywords[text] {
				bad(key, "a Go package name (lower-case letters and digits)")
				continue
			}
			opts.pkg = text
		case "version":
			if !semver.MatchString(text) {
				bad(key, "a version such as 1.2.3")
				continue
			}
			opts.version = text
		case "runtime_version":
			if !strings.HasPrefix(text, "v") || !semver.MatchString(text[1:]) {
				bad(key, "a module version such as v0.1.0")
				continue
			}
			opts.runtimeVersion = text
		case "runtime_path":
			if text == "" || strings.ContainsAny(text, " \t\n\"") {
				bad(key, "a directory path without spaces")
				continue
			}
			opts.runtimePath = text
		default:
			diags = append(diags, diagnostic{Code: "GO004", Severity: "error",
				Message: fmt.Sprintf("unknown option `%s`", key),
				Help:    "The go target takes module, package, version, runtime_version and runtime_path."})
		}
	}
	return opts, diags
}

type request struct {
	Protocol int                        `json:"protocol"`
	Target   string                     `json:"target"`
	Options  map[string]json.RawMessage `json:"options"`
	IR       IR                         `json:"ir"`
}

type response struct {
	Protocol    int           `json:"protocol"`
	Files       []emittedFile `json:"files"`
	Diagnostics []diagnostic  `json:"diagnostics"`
}

// emit turns a request into the response.
func emit(req *request) response {
	out := response{Protocol: protocol, Files: []emittedFile{}, Diagnostics: []diagnostic{}}
	opts, diags := parseOptions(req.Options, &req.IR)
	out.Diagnostics = append(out.Diagnostics, diags...)
	if len(diags) > 0 {
		return out
	}
	g := newGen(&req.IR, opts)
	files, err := g.run()
	out.Diagnostics = append(out.Diagnostics, g.diags...)
	if err != nil {
		out.Diagnostics = append(out.Diagnostics, diagnostic{Code: "GO005", Severity: "error",
			Message: "the generated Go source is invalid: " + err.Error(),
			Help:    "This is a bug in tungsten-emit-go; report it with the IR (tungsten ir dump)."})
		return out
	}
	out.Files = files
	return out
}

func write(v any) int {
	w := bufio.NewWriter(os.Stdout)
	enc := json.NewEncoder(w)
	enc.SetEscapeHTML(false)
	if err := enc.Encode(v); err != nil {
		fmt.Fprintf(os.Stderr, "cannot write the response: %v\n", err)
		return 1
	}
	if err := w.Flush(); err != nil {
		fmt.Fprintf(os.Stderr, "cannot write the response: %v\n", err)
		return 1
	}
	return 0
}

func run(args []string, stdin io.Reader) int {
	switch {
	case len(args) == 1 && args[0] == "--describe":
		return write(describe())
	case len(args) != 0:
		fmt.Fprintln(os.Stderr, "usage: tungsten-emit-go [--describe]  (the request is read from stdin)")
		return 2
	}
	data, err := io.ReadAll(stdin)
	if err != nil {
		fmt.Fprintf(os.Stderr, "cannot read the request: %v\n", err)
		return 1
	}
	var head struct {
		Protocol int `json:"protocol"`
	}
	if err := json.Unmarshal(data, &head); err != nil {
		fmt.Fprintf(os.Stderr, "the request is not JSON: %v\n", err)
		return 1
	}
	if head.Protocol != protocol {
		fmt.Fprintf(os.Stderr, "this emitter speaks protocol %d, the request says %d\n", protocol, head.Protocol)
		return 1
	}
	var req request
	if err := json.Unmarshal(data, &req); err != nil {
		fmt.Fprintf(os.Stderr, "the request does not hold a readable IR: %v\n", err)
		return 1
	}
	return write(emit(&req))
}

func main() {
	os.Exit(run(os.Args[1:], os.Stdin))
}
