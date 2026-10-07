# Template: scripts/homebrew_formula.py fills the version, repository URL and
# sha256 markers (names in at-signs) from a release's SHA256SUMS. The release workflow
# attaches the result to each release as gsql-lsp.rb; copy it to
# Formula/gsql-lsp.rb of a tap repository (homebrew-gsql-lsp).
class GsqlLsp < Formula
  desc "Language server for TigerGraph GSQL"
  homepage "@REPO_URL@"
  license "MIT"

  on_macos do
    on_arm do
      url "@REPO_URL@/releases/download/v@VERSION@/gsql-lsp-aarch64-apple-darwin.tar.gz"
      sha256 "@SHA256_aarch64-apple-darwin@"
    end
    on_intel do
      url "@REPO_URL@/releases/download/v@VERSION@/gsql-lsp-x86_64-apple-darwin.tar.gz"
      sha256 "@SHA256_x86_64-apple-darwin@"
    end
  end

  on_linux do
    on_arm do
      url "@REPO_URL@/releases/download/v@VERSION@/gsql-lsp-aarch64-unknown-linux-musl.tar.gz"
      sha256 "@SHA256_aarch64-unknown-linux-musl@"
    end
    on_intel do
      url "@REPO_URL@/releases/download/v@VERSION@/gsql-lsp-x86_64-unknown-linux-musl.tar.gz"
      sha256 "@SHA256_x86_64-unknown-linux-musl@"
    end
  end

  def install
    # The archive holds gsql-lsp-<target>/{gsql-lsp,README.md,LICENSE}; Homebrew
    # enters the single top-level directory.
    bin.install "gsql-lsp"
  end

  test do
    assert_match "gsql-lsp #{version}", shell_output("#{bin}/gsql-lsp --version")
  end
end
