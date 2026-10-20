// Populate the sidebar
//
// This is a script, and not included directly in the page, to control the total size of the book.
// The TOC contains an entry for each page, so if each page includes a copy of the TOC,
// the total size of the page becomes O(n**2).
class MDBookSidebarScrollbox extends HTMLElement {
    constructor() {
        super();
    }
    connectedCallback() {
        this.innerHTML = '<ol class="chapter"><li class="chapter-item expanded affix "><a href="index.html">Introduction</a></li><li class="chapter-item expanded "><a href="walkthrough.html"><strong aria-hidden="true">1.</strong> v0.1.3 walkthrough</a></li><li class="chapter-item expanded "><a href="getting-started.html"><strong aria-hidden="true">2.</strong> Getting started</a></li><li class="chapter-item expanded "><a href="platforms.html"><strong aria-hidden="true">3.</strong> Supported platforms</a></li><li class="chapter-item expanded "><a href="package-managers.html"><strong aria-hidden="true">4.</strong> Package managers and containers</a></li><li class="chapter-item expanded "><a href="project-setup.html"><strong aria-hidden="true">5.</strong> Project setup and doctor</a></li><li class="chapter-item expanded "><a href="importing.html"><strong aria-hidden="true">6.</strong> Importing API descriptions and saved requests</a></li><li class="chapter-item expanded "><a href="compatibility.html"><strong aria-hidden="true">7.</strong> Specification compatibility</a></li><li class="chapter-item expanded "><a href="runtime-distribution.html"><strong aria-hidden="true">8.</strong> Runtime distribution</a></li><li class="chapter-item expanded "><a href="runtime-benchmarks.html"><strong aria-hidden="true">9.</strong> Runtime benchmarks</a></li><li><ol class="section"><li class="chapter-item expanded "><a href="runtime-benchmark-results.html"><strong aria-hidden="true">9.1.</strong> Versioned results</a></li></ol></li><li class="chapter-item expanded "><a href="changelog.html"><strong aria-hidden="true">10.</strong> Changelog</a></li><li class="chapter-item expanded affix "><li class="part-title">Guides</li><li class="chapter-item expanded "><a href="manifests.html"><strong aria-hidden="true">11.</strong> Manifest reference</a></li><li class="chapter-item expanded "><a href="safety.html"><strong aria-hidden="true">12.</strong> Safety and runtime behaviour</a></li><li class="chapter-item expanded "><a href="mcp.html"><strong aria-hidden="true">13.</strong> MCP server guide</a></li><li class="chapter-item expanded "><a href="webmcp.html"><strong aria-hidden="true">14.</strong> Browser WebMCP guide</a></li><li class="chapter-item expanded "><a href="browser-integration.html"><strong aria-hidden="true">15.</strong> Browser lifecycle and frameworks</a></li><li class="chapter-item expanded "><a href="tool-results.html"><strong aria-hidden="true">16.</strong> Streaming and binary tool results</a></li><li class="chapter-item expanded "><a href="migrations.html"><strong aria-hidden="true">17.</strong> SDK regeneration and migration</a></li><li class="chapter-item expanded "><a href="tool-catalog.html"><strong aria-hidden="true">18.</strong> Tool catalogue and external emitters</a></li><li class="chapter-item expanded "><a href="sdk/index.html"><strong aria-hidden="true">19.</strong> SDK guides</a></li><li><ol class="section"><li class="chapter-item expanded "><a href="sdk/typescript.html"><strong aria-hidden="true">19.1.</strong> TypeScript</a></li><li class="chapter-item expanded "><a href="sdk/python.html"><strong aria-hidden="true">19.2.</strong> Python</a></li><li class="chapter-item expanded "><a href="sdk/rust.html"><strong aria-hidden="true">19.3.</strong> Rust SDK and CLI</a></li><li class="chapter-item expanded "><a href="sdk/go.html"><strong aria-hidden="true">19.4.</strong> Go</a></li><li class="chapter-item expanded "><a href="sdk/java.html"><strong aria-hidden="true">19.5.</strong> Java</a></li><li class="chapter-item expanded "><a href="sdk/kotlin.html"><strong aria-hidden="true">19.6.</strong> Kotlin</a></li><li class="chapter-item expanded "><a href="sdk/csharp.html"><strong aria-hidden="true">19.7.</strong> C#</a></li><li class="chapter-item expanded "><a href="sdk/fsharp.html"><strong aria-hidden="true">19.8.</strong> F#</a></li><li class="chapter-item expanded "><a href="sdk/ruby.html"><strong aria-hidden="true">19.9.</strong> Ruby</a></li><li class="chapter-item expanded "><a href="sdk/dart.html"><strong aria-hidden="true">19.10.</strong> Dart</a></li><li class="chapter-item expanded "><a href="sdk/docs.html"><strong aria-hidden="true">19.11.</strong> Docs target</a></li></ol></li><li class="chapter-item expanded "><li class="part-title">Reference</li><li class="chapter-item expanded "><a href="reference/cli.html"><strong aria-hidden="true">20.</strong> CLI reference</a></li><li class="chapter-item expanded "><a href="reference/diagnostics.html"><strong aria-hidden="true">21.</strong> Diagnostic codes</a></li><li class="chapter-item expanded affix "><li class="part-title">Case study</li><li class="chapter-item expanded "><a href="case-study.html"><strong aria-hidden="true">22.</strong> The ZROtext pilot</a></li><li class="chapter-item expanded "><a href="downloads.html"><strong aria-hidden="true">23.</strong> Downloads: llms.txt</a></li></ol>';
        // Set the current, active page, and reveal it if it's hidden
        let current_page = document.location.href.toString().split("#")[0].split("?")[0];
        if (current_page.endsWith("/")) {
            current_page += "index.html";
        }
        var links = Array.prototype.slice.call(this.querySelectorAll("a"));
        var l = links.length;
        for (var i = 0; i < l; ++i) {
            var link = links[i];
            var href = link.getAttribute("href");
            if (href && !href.startsWith("#") && !/^(?:[a-z+]+:)?\/\//.test(href)) {
                link.href = path_to_root + href;
            }
            // The "index" page is supposed to alias the first chapter in the book.
            if (link.href === current_page || (i === 0 && path_to_root === "" && current_page.endsWith("/index.html"))) {
                link.classList.add("active");
                var parent = link.parentElement;
                if (parent && parent.classList.contains("chapter-item")) {
                    parent.classList.add("expanded");
                }
                while (parent) {
                    if (parent.tagName === "LI" && parent.previousElementSibling) {
                        if (parent.previousElementSibling.classList.contains("chapter-item")) {
                            parent.previousElementSibling.classList.add("expanded");
                        }
                    }
                    parent = parent.parentElement;
                }
            }
        }
        // Track and set sidebar scroll position
        this.addEventListener('click', function(e) {
            if (e.target.tagName === 'A') {
                sessionStorage.setItem('sidebar-scroll', this.scrollTop);
            }
        }, { passive: true });
        var sidebarScrollTop = sessionStorage.getItem('sidebar-scroll');
        sessionStorage.removeItem('sidebar-scroll');
        if (sidebarScrollTop) {
            // preserve sidebar scroll position when navigating via links within sidebar
            this.scrollTop = sidebarScrollTop;
        } else {
            // scroll sidebar to current active section when navigating via "next/previous chapter" buttons
            var activeSection = document.querySelector('#sidebar .active');
            if (activeSection) {
                activeSection.scrollIntoView({ block: 'center' });
            }
        }
        // Toggle buttons
        var sidebarAnchorToggles = document.querySelectorAll('#sidebar a.toggle');
        function toggleSection(ev) {
            ev.currentTarget.parentElement.classList.toggle('expanded');
        }
        Array.from(sidebarAnchorToggles).forEach(function (el) {
            el.addEventListener('click', toggleSection);
        });
    }
}
window.customElements.define("mdbook-sidebar-scrollbox", MDBookSidebarScrollbox);
