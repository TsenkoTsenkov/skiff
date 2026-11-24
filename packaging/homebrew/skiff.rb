# Homebrew cask for Skiff. Template: copy it to Casks/skiff.rb in the tap
# repository (TsenkoTsenkov/homebrew-tap) and, for each stable release, set
# `version` and `sha256` (the line for Skiff-macos-universal.dmg in the
# release's SHA256SUMS). packaging/README.md has the steps.
#
#   brew install --cask tsenkotsenkov/tap/skiff
cask "skiff" do
  version "0.1.0"
  sha256 "0000000000000000000000000000000000000000000000000000000000000000"

  url "https://github.com/TsenkoTsenkov/skiff/releases/download/v#{version}/Skiff-macos-universal.dmg"
  name "Skiff"
  desc "Fast, native Kubernetes cluster browser"
  homepage "https://github.com/TsenkoTsenkov/skiff"

  livecheck do
    url :url
    strategy :github_latest
  end

  # The app updates itself (stable channel), so brew does not need to.
  auto_updates true
  depends_on macos: ">= :big_sur"

  app "Skiff.app"

  zap trash: [
    "~/.config/skiff",
    "~/Library/Application Support/skiff",
    "~/Library/Caches/io.github.tsenkotsenkov.skiff",
    "~/Library/HTTPStorages/io.github.tsenkotsenkov.skiff",
    "~/Library/Preferences/io.github.tsenkotsenkov.skiff.plist",
    "~/Library/Saved Application State/io.github.tsenkotsenkov.skiff.savedState",
  ]
end
