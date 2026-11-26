// Cook-only copying. Work on anonymous layer clones and owned scratch files,
// including dormant variant specs and every (possibly nested) USDZ member.
// No source layer or live Sdf registry entry is edited.
static bool copy_runtime_asset_locked(const std::filesystem::path& source,
                                      const std::filesystem::path& destination,
                                      unsigned depth) {
    namespace fs = std::filesystem;
    if (depth > 32) throw std::runtime_error("runtime package nesting exceeds 32");
    const std::string extension = lower_extension(source);
    if (extension == ".usd" || extension == ".usda" || extension == ".usdc") {
        const auto original = SdfLayer::FindOrOpen(source.string());
        const auto copy = SdfLayer::CreateAnonymous(extension);
        if (!original || !copy) throw std::runtime_error("cannot read runtime layer " + source.string());
        copy->TransferContent(original);
        bool changed = false;
        std::vector<SdfPath> remove;
        copy->Traverse(SdfPath::AbsoluteRootPath(), [&](const SdfPath& path) {
            if (!path.IsPropertyPath() || path.GetName() != "idtx:behavior:source") return;
            const auto spec = copy->GetAttributeAtPath(path);
            if (!spec) return;
            const VtValue value = spec->GetDefaultValue();
            // Leave the empty schema declaration intact in generatedSchema.usda.
            if ((!value.IsEmpty() && (!value.IsHolding<SdfAssetPath>() ||
                 !value.UncheckedGet<SdfAssetPath>().GetAssetPath().empty())) ||
                !copy->ListTimeSamplesForPath(path).empty()) remove.push_back(path);
        });
        for (const auto& path : remove) {
            // GetPrimPath strips a variant selection on the owning prim;
            // GetParentPath retains it and addresses the actual variant spec.
            const auto owner = copy->GetPrimAtPath(path.GetParentPath());
            if (!owner) throw std::runtime_error("cannot locate runtime source property owner");
            owner->RemoveProperty(copy->GetPropertyAtPath(path));
            if (copy->GetPropertyAtPath(path)) throw std::runtime_error("cannot remove runtime source binding");
            changed = true;
        }
        UsdUtilsModifyAssetPaths(copy, [&](const std::string& asset) {
            if (is_behavior_source_asset(asset)) { changed = true; return std::string(); }
            return asset;
        });
        if (changed) {
            if (!copy->Export(destination.string())) throw std::runtime_error("cannot export runtime layer");
        } else fs::copy_file(source, destination, fs::copy_options::overwrite_existing);
        return changed;
    }
    if (extension != ".usdz") {
        fs::copy_file(source, destination, fs::copy_options::overwrite_existing);
        return false;
    }
    const auto archive = SdfZipFile::Open(source.string());
    if (!archive) throw std::runtime_error("cannot read runtime USDZ " + source.string());
    // Destination is owned by the caller; recursive scratch is on the same
    // volume and cannot overlap any authoring or package member path.
    const fs::path scratch = destination.string() + ".runtime-members";
    if (!fs::create_directory(scratch)) throw std::runtime_error("runtime scratch already exists");
    std::vector<std::pair<fs::path, std::string>> members;
    bool changed = false;
    try {
        std::size_t index = 0;
        for (auto entry = archive.begin(); entry != archive.end(); ++entry, ++index) {
            const std::string name = *entry;
            if (!safe_package_member(fs::path(name))) throw std::runtime_error("unsafe runtime USDZ member");
            const auto info = entry.GetFileInfo();
            if (info.compressionMethod != 0 || info.encrypted) throw std::runtime_error("invalid runtime USDZ encoding");
            if (is_behavior_source_asset(name)) { changed = true; continue; }
            // Numbered scratch leaves avoid collisions with archive-authored
            // directory names while AddFile preserves the exact member order/name.
            const std::string ext = lower_extension(fs::path(name));
            const fs::path input = scratch / (std::to_string(index) + "-input" + ext);
            const fs::path output = scratch / (std::to_string(index) + "-output" + ext);
            {
                std::ofstream stream(input, std::ios::binary | std::ios::trunc);
                stream.write(entry.GetFile(), static_cast<std::streamsize>(info.size));
                if (!stream) throw std::runtime_error("cannot extract runtime USDZ member");
            }
            changed = copy_runtime_asset_locked(input, output, depth + 1) || changed;
            members.emplace_back(output, name);
        }
        if (changed) {
            if (members.empty() || members.front().second != *archive.begin())
                throw std::runtime_error("runtime USDZ lost its root layer");
            SdfZipFileWriter writer = SdfZipFileWriter::CreateNew(destination.string());
            if (!writer) throw std::runtime_error("cannot create runtime USDZ");
            for (const auto& [file, name] : members) {
                // USD's ZIP writer uses host-local file mtimes. Pick the middle
                // of the first DOS two-second interval on every host, including
                // nested archives, so no wall-clock value enters cooked bytes.
                std::tm epoch{};
                epoch.tm_year = 80; epoch.tm_mon = 0; epoch.tm_mday = 1;
                epoch.tm_sec = 1; epoch.tm_isdst = -1;
                const auto target = std::chrono::system_clock::from_time_t(std::mktime(&epoch));
                fs::last_write_time(file, fs::file_time_type::clock::now() +
                    std::chrono::duration_cast<fs::file_time_type::duration>(target - std::chrono::system_clock::now()));
                if (writer.AddFile(file.string(), name).empty()) {
                    writer.Discard();
                    throw std::runtime_error("cannot add runtime USDZ member");
                }
            }
            if (!writer.Save()) throw std::runtime_error("cannot finalize runtime USDZ");
        } else fs::copy_file(source, destination, fs::copy_options::overwrite_existing);
        fs::remove_all(scratch);
    } catch (...) {
        std::error_code ignored;
        fs::remove_all(scratch, ignored);
        fs::remove(destination, ignored);
        throw;
    }
    return changed;
}

bool copy_runtime_asset(rust::Str source, rust::Str destination) {
    const std::string input = to_std(source), output = to_std(destination);
    if (input.empty() || output.empty() || input == output)
        throw std::runtime_error("runtime copy requires distinct source and destination");
    return with_stage_lifecycle_lock<std::unique_lock<std::shared_mutex>>([&] {
        return copy_runtime_asset_locked(input, output, 0);
    });
}
