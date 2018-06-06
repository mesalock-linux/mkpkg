env:
  pkgname: linux
  pkgver: 4.16.14

package:
  name: $pkgname
  version: $pkgver
  description: Linux kernel
  license: [GPL2]

  source:
    - https://cdn.kernel.org/pub/$pkgname/kernel/v4.x/$pkgname-$pkgver.tar.xz
    - config

  prepare:
    - mkdir -p build
    - cp config build/.config
    - make -C "${pkgname}-${pkgver}" O="$(pwd)/build" ARCH="x86_64" silentoldconfig

  build:
    - cd build && make ARCH=x86_64 CC=gcc KBUILD_BUILD_VERSION="${pkgver}-Mesalock" bzImage

  install:
    - install -Dm644 build/arch/x86_64/boot/bzImage "$pkgdir"/boot/vmlinuz
