// C++ implementation of idtx::materialx::WgslShaderGenerator — MaterialX GLSL
// generation using MaterialX::GlslShaderGenerator (MaterialXGenGlsl 1.39.5) feeding
// the Rust-side naga GLSL→WGSL transpile path.
// Only compiled when IDTX_HAVE_MATERIALX_WGSL is defined.
//
// Note: MaterialX::WgslShaderGenerator in OpenUSD 26.08 produces Vulkan GLSL (#version 450),
// not native WGSL. We use GlslShaderGenerator directly — the proven base class — and always
// route through naga for GLSL→WGSL transpilation. requires_phase2 is always set to true.

#pragma once

#include <pxr/usd/usd/stage.h>
#include <pxr/usd/usd/primRange.h>
#include <pxr/usd/sdf/path.h>
#include <pxr/usd/sdf/layer.h>
#include <pxr/usd/usdShade/material.h>

#include <MaterialXGenGlsl/GlslShaderGenerator.h>
#include <MaterialXGenShader/DefaultColorManagementSystem.h>
#include <MaterialXGenShader/GenContext.h>
#include <MaterialXGenShader/TypeDesc.h>
#include <MaterialXFormat/Util.h>
#include <MaterialXFormat/XmlIo.h>
#include <MaterialXCore/Document.h>
#include <MaterialXCore/Node.h>

#include <functional>
#include <sstream>
#include <string>
#include <vector>

PXR_NAMESPACE_USING_DIRECTIVE

namespace idtx { namespace materialx {

struct WgslResult {
    std::string              wgsl_source;
    std::string              fingerprint;
    bool                     requires_phase2 = false;
    std::vector<std::string> diagnostics;
};

// Load the MaterialX standard library once per process.
// install_dir must be the OpenUSD/MaterialX install prefix (IDTX_USD_DIR).
static MaterialX::DocumentPtr load_stdlib(const std::string& install_dir) {
    static std::mutex mtx;
    static std::shared_ptr<MaterialX::Document> cached;
    static std::string cached_key;
    std::lock_guard<std::mutex> lock(mtx);
    if (cached && cached_key == install_dir) {
        return cached;
    }
    auto stdlib = MaterialX::createDocument();
    MaterialX::FileSearchPath search;
    search.append(MaterialX::FilePath(install_dir));
    MaterialX::loadLibraries({"libraries"}, search, stdlib);
    cached = stdlib;
    cached_key = install_dir;
    return stdlib;
}

// Return the install directory: IDTX_USD_DIR env var, or derived from the
// .mtlx layer path (strips everything at and after '/libraries/').
static std::string resolve_install_dir(const std::string& mtlx_path) {
    if (const char* env = std::getenv("IDTX_USD_DIR")) {
        return env;
    }
    const std::string marker = "/libraries/";
    const std::size_t pos = mtlx_path.find(marker);
    if (pos != std::string::npos) {
        return mtlx_path.substr(0, pos);
    }
    return {};
}

class WgslShaderGenerator {
public:
    static WgslResult Generate(const UsdStageRefPtr& stage,
                               const SdfPath&        material_path)
    {
        WgslResult result;

        const UsdPrim prim = stage->GetPrimAtPath(material_path);
        if (!prim || !prim.IsA<UsdShadeMaterial>()) {
            result.requires_phase2 = true;
            result.diagnostics.push_back(
                "WgslShaderGenerator: not a Material: " +
                material_path.GetString());
            return result;
        }

        // Walk the prim stack to find the .mtlx source layer.
        std::string mtlx_layer_id;
        std::string material_name_in_doc;
        for (const auto& spec : prim.GetPrimStack()) {
            const std::string id = spec->GetLayer()->GetIdentifier();
            if (id.size() >= 5 &&
                id.compare(id.size() - 5, 5, ".mtlx") == 0) {
                mtlx_layer_id = id;
                // The spec path name is the prim name inside the .mtlx layer.
                material_name_in_doc = spec->GetPath().GetName();
                break;
            }
        }

        if (mtlx_layer_id.empty()) {
            // No .mtlx layer — fall back so naga can handle it.
            result.requires_phase2 = true;
            result.diagnostics.push_back(
                "WgslShaderGenerator: no .mtlx layer for " +
                material_path.GetString());
            return result;
        }

        // Load the MaterialX document for this .mtlx file.
        MaterialX::DocumentPtr doc;
        try {
            doc = MaterialX::createDocument();
            MaterialX::readFromXmlFile(doc, mtlx_layer_id);
        } catch (const std::exception& e) {
            result.requires_phase2 = true;
            result.diagnostics.push_back(
                std::string("WgslShaderGenerator: readFromXmlFile failed (") +
                mtlx_layer_id + "): " + e.what());
            return result;
        }

        // Import the stdlib into the document.
        const std::string install_dir = resolve_install_dir(mtlx_layer_id);
        if (install_dir.empty()) {
            result.diagnostics.push_back(
                "WgslShaderGenerator: IDTX_USD_DIR not set and could not "
                "derive install dir from " + mtlx_layer_id);
        }
        try {
            auto stdlib = load_stdlib(install_dir);
            doc->importLibrary(stdlib);
        } catch (const std::exception& e) {
            result.diagnostics.push_back(
                std::string("WgslShaderGenerator: stdlib import warning: ") +
                e.what());
            // Non-fatal; generation may still succeed for simple materials.
        }

        // Find the surfacematerial node in the document.
        MaterialX::NodePtr mat_node;

        // Try exact name match first (USD prim name should equal the mtlx element name).
        const std::vector<std::string> candidates = {
            material_name_in_doc,
            material_path.GetName(), // last component of USD path
        };
        for (const auto& name : candidates) {
            if (name.empty()) continue;
            auto node = doc->getNode(name);
            if (node && node->getCategory() == "surfacematerial") {
                mat_node = node;
                break;
            }
        }

        // If still not found, pick the first surfacematerial (works for
        // single-material .mtlx files).
        if (!mat_node) {
            auto nodes = doc->getNodes("surfacematerial");
            if (!nodes.empty()) {
                mat_node = nodes.front();
                result.diagnostics.push_back(
                    "WgslShaderGenerator: name match failed for '" +
                    material_name_in_doc +
                    "', falling back to first surfacematerial '" +
                    mat_node->getName() + "'");
            }
        }

        if (!mat_node) {
            result.requires_phase2 = true;
            result.diagnostics.push_back(
                "WgslShaderGenerator: no surfacematerial in " + mtlx_layer_id);
            return result;
        }

        // Set up search paths for texture resolution and stdlib.
        // GenContext needs the install root so that relative includes like
        // 'libraries/stdlib/genglsl/lib/mx_math.glsl' resolve correctly.
        MaterialX::FileSearchPath ctx_search;
        if (!install_dir.empty()) {
            ctx_search.append(MaterialX::FilePath(install_dir));
        }
        // Also add the directory of the .mtlx file for relative texture paths.
        {
            MaterialX::FilePath fp(mtlx_layer_id);
            ctx_search.append(fp.getParentPath());
        }

        // Generate GLSL via GlslShaderGenerator — the proven base-class generator.
        // Output is always Vulkan GLSL (#version 450) which the Rust naga path
        // transpiles to WGSL. requires_phase2 is unconditionally true here.
        try {
            auto gen = std::make_shared<MaterialX::GlslShaderGenerator>(
                MaterialX::TypeSystem::create());

            auto cms = MaterialX::DefaultColorManagementSystem::create(
                gen->getTarget());
            cms->loadLibrary(doc);
            gen->setColorManagementSystem(cms);

            MaterialX::GenContext ctx(
                std::static_pointer_cast<MaterialX::ShaderGenerator>(gen));
            ctx.registerSourceCodeSearchPath(ctx_search);
            ctx.getOptions().targetColorSpaceOverride = "lin_rec709";
            ctx.getOptions().fileTextureVerticalFlip  = false;

            auto shader = gen->generate(mat_node->getName(), mat_node, ctx);
            const std::string& src =
                shader->getSourceCode(MaterialX::Stage::PIXEL);

            result.wgsl_source    = src;

            // GlslShaderGenerator always emits Vulkan GLSL — route to naga.
            result.requires_phase2 = true;
            result.diagnostics.push_back(
                "GlslShaderGenerator: produced GLSL (Stage::PIXEL, " +
                std::to_string(src.size()) +
                " bytes) \xe2\x80\x94 routing to naga GLSL\xe2\x86\x92WGSL transpile");

            // Fingerprint: hex hash of the WGSL source.
            std::size_t h = std::hash<std::string>{}(src);
            std::ostringstream fp;
            fp << std::hex << h;
            result.fingerprint = fp.str();

        } catch (const std::exception& e) {
            result.requires_phase2 = true;
            result.diagnostics.push_back(
                std::string("GlslShaderGenerator: generate failed: ") +
                e.what());
        }

        return result;
    }
};

}} // namespace idtx::materialx
