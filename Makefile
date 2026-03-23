.PHONY: build build-rust generate-xcode build-xcode build-xcode-debug clean run

ARCH := aarch64-apple-darwin
XCODE_APP_DIR := KoeApp
XCODE_PROJ := $(XCODE_APP_DIR)/Koe.xcodeproj
XCODE_SCHEME := Koe

build: build-rust build-xcode

build-rust:
	cargo build --manifest-path koe-core/Cargo.toml --release --target $(ARCH)

generate-xcode:
	@command -v xcodegen >/dev/null 2>&1 || { echo "error: xcodegen is required (brew install xcodegen)"; exit 1; }
	cd $(XCODE_APP_DIR) && xcodegen

build-xcode: generate-xcode
	cd $(XCODE_APP_DIR) && xcodebuild -project Koe.xcodeproj -scheme $(XCODE_SCHEME) -configuration Release build

build-xcode-debug: generate-xcode
	cd $(XCODE_APP_DIR) && xcodebuild -project Koe.xcodeproj -scheme $(XCODE_SCHEME) -configuration Debug build

clean:
	cargo clean
	@if [ -d "$(XCODE_PROJ)" ]; then \
		cd $(XCODE_APP_DIR) && xcodebuild -project Koe.xcodeproj -scheme $(XCODE_SCHEME) clean; \
	else \
		echo "Skipping Xcode clean: $(XCODE_PROJ) does not exist"; \
	fi

run: build-xcode-debug
	@APP_PATH="$$(xcodebuild -project $(XCODE_PROJ) -scheme $(XCODE_SCHEME) -configuration Debug -showBuildSettings 2>/dev/null | awk -F ' = ' '/TARGET_BUILD_DIR/{dir=$$2} /FULL_PRODUCT_NAME/{name=$$2} END{print dir "/" name}')"; \
	if [ -z "$$APP_PATH" ] || [ ! -d "$$APP_PATH" ]; then \
		echo "error: built app not found at $$APP_PATH"; \
		exit 1; \
	fi; \
	open "$$APP_PATH"
