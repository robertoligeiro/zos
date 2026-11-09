PYTHON ?= python

.PHONY: fmt build image run run-bios debug check test
fmt:
	$(PYTHON) tools\zos.py fmt
build:
	$(PYTHON) tools\zos.py build
image:
	$(PYTHON) tools\zos.py image
run:
	$(PYTHON) tools\zos.py run --firmware uefi
run-bios:
	$(PYTHON) tools\zos.py run --firmware bios
debug:
	$(PYTHON) tools\zos.py debug
check:
	$(PYTHON) tools\zos.py check
test:
	$(PYTHON) tools\zos.py test
