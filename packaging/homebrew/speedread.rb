# Homebrew formula (for a tap such as brennengreen/homebrew-tap).
# Builds from source, so no notarization is needed.
#
# Name collision: homebrew-core already has an unrelated `speedread` formula
# (pasky/speedread, an RSVP speed-reading tool), and both install
# bin/speedread. Plain `brew install speedread` installs that one, so document
# the fully qualified command: `brew install brennengreen/tap/speedread`.
class Speedread < Formula
  desc "Token-efficient code search and navigation for AI coding agents (MCP)"
  homepage "https://github.com/brennengreen/speedread"
  url "https://github.com/brennengreen/speedread/archive/refs/tags/v0.1.0.tar.gz"
  sha256 "REPLACE_WITH_RELEASE_TARBALL_SHA256"
  license "MIT"
  head "https://github.com/brennengreen/speedread.git", branch: "main"

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args
  end

  test do
    (testpath/"a.rs").write("fn main() {\n    println!(\"hi\");\n}\n")
    assert_match "fn main()", shell_output("#{bin}/speedread --root #{testpath} read a.rs#main")
  end
end
