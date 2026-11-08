# Template: rendered by .github/workflows/package-manifests.yml. Placeholders between at signs are filled in.
# For a tap of the owner's own (for example ulaas/homebrew-tap, as Casks/gitem.rb). The binary isn't
# signed or notarized, which homebrew/cask itself no longer accepts.
cask "gitem" do
  version "@VERSION@"
  sha256 "@SHA256_MACOS_UNIVERSAL_TGZ@"

  url "https://github.com/ulaas/gitem/releases/download/v#{version}/gitem-#{version}-macos-universal.tar.gz"
  name "gitem"
  desc "Desktop Git client with a commit graph, editor, and diff and merge tools"
  homepage "https://ulaas.github.io/gitem/"

  livecheck do
    url :url
    strategy :github_latest
  end

  depends_on formula: "git"

  binary "gitem-#{version}-macos-universal/gitem"

  zap trash: [
    "~/Library/Application Support/dev.gitem.gitem",
  ]

  caveats <<~EOS
    gitem isn't signed or notarized. If macOS refuses to start it, run:
      xattr -d com.apple.quarantine "#{HOMEBREW_PREFIX}/bin/gitem"
  EOS
end
