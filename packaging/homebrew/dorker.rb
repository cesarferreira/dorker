# Homebrew formula template. Fill in `version` and `sha256` from the GitHub release
# assets (`shasum -a 256 dorker-aarch64-apple-darwin.tar.gz`), then publish it to a
# tap (e.g. cesarferreira/homebrew-tap). Not yet published.
class Dorker < Formula
  desc "Docker for headless Android emulator environments"
  homepage "https://github.com/cesarferreira/dorker"
  version "REPLACE_AT_RELEASE"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/cesarferreira/dorker/releases/download/v#{version}/dorker-aarch64-apple-darwin.tar.gz"
      sha256 "REPLACE_AT_RELEASE"
    end
  end

  def install
    bin.install "dorker"
  end

  def caveats
    <<~EOS
      dorker needs the Android SDK (emulator + platform-tools) and a system image.
      Run `dorker doctor` to check your setup.
    EOS
  end

  test do
    assert_match "dorker", shell_output("#{bin}/dorker --version")
  end
end
