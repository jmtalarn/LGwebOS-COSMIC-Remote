pkgname=lg-webos-cosmic
pkgver=0.1.0
pkgrel=1
pkgdesc='COSMIC panel applet for controlling LG webOS TVs'
arch=('x86_64' 'aarch64')
url='https://github.com/jmtalarn/LGwebOS-COSMIC-Remote'
license=('MIT')
depends=('openssl' 'libsecret')
makedepends=('cargo' 'git' 'libxkbcommon' 'pkgconf' 'wayland')
source=()
sha256sums=()

prepare() {
  cd "${startdir}"
  export RUSTUP_TOOLCHAIN=stable
  cargo fetch --locked
}

build() {
  cd "${startdir}"
  export RUSTUP_TOOLCHAIN=stable
  cargo build --release --locked
}

package() {
  cd "${startdir}"
  install -Dm755 target/release/lg-webos-cosmic "${pkgdir}/usr/bin/lg-webos-cosmic"
  install -Dm644 resources/io.github.tvmenubar.LGWebOSRemote.desktop \
    "${pkgdir}/usr/share/applications/io.github.tvmenubar.LGWebOSRemote.desktop"
  install -Dm644 resources/io.github.tvmenubar.LGWebOSRemote.svg \
    "${pkgdir}/usr/share/icons/hicolor/scalable/apps/io.github.tvmenubar.LGWebOSRemote.svg"
  install -Dm644 LICENSE "${pkgdir}/usr/share/licenses/${pkgname}/LICENSE"
}
