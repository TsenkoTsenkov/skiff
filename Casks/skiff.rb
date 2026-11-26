# Homebrew cask for Skiff. This repository is its own tap:
#
#   brew install --cask tsenkotsenkov/skiff/skiff
#
# (which taps https://github.com/TsenkoTsenkov/skiff first; the explicit form
# is `brew tap tsenkotsenkov/skiff https://github.com/TsenkoTsenkov/skiff`).
#
# hack/bump-version.py sets `version` with every stable release. There is no
# fixed sha256 because the checksum is only known once the release workflow
# has built the files; the download is the signed and notarized app, and
# macOS checks that signature (Gatekeeper) before it first opens. The hash of
# every file is in the release's SHA256SUMS. Right after a version bump is
# merged, the release takes about half an hour to be published; until then
# this cask names a download that does not exist yet.
cask "skiff" do
  version "0.1.4"
  sha256 :no_check

  url "https://github.com/TsenkoTsenkov/skiff/releases/download/v#{version}/Skiff-macos-universal.dmg"
  name "Skiff"
  desc "Fast, native Kubernetes desktop client"
  homepage "https://tsenkotsenkov.github.io/skiff/"

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
