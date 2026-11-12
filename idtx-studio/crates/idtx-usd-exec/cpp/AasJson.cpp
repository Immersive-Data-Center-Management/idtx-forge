// Vendored from the sibling `idtx-flow-idcm` project (usd/source/AasJson.cpp).
// See AasJson.h for the provenance note and API documentation.
#include "AasJson.h"

#include <cctype>
#include <iostream>
#include <string>

#include <pxr/base/js/types.h>

PXR_NAMESPACE_USING_DIRECTIVE

namespace idtx::aas_json
{
    namespace
    {
        /**
         * @brief Returns true if @p value is a JSON number (real or integer).
         */
        bool isNumber(const JsValue& value)
        {
            return value.IsReal() || value.IsInt() || value.IsUInt64();
        }

        /**
         * @brief Returns @p value as a double, assuming it is numeric.
         */
        double asDouble(const JsValue& value)
        {
            if (value.IsReal()) return value.GetReal();
            if (value.IsInt())  return static_cast<double>(value.GetInt());
            if (value.IsUInt64()) return static_cast<double>(value.GetUInt64());
            return 0.0;
        }

        /**
         * @brief Returns the AAS child array of @p parent, or nullptr.
         *
         * AAS Submodels expose their children under @c submodelElements;
         * SubmodelElementCollections expose them under @c value. This is the
         * single canonical implementation of the container-duality rule that
         * used to live in @c AasPositional::childArray().
         *
         * Array-aware: when @p parent is itself an array (e.g. the
         * @c outputArguments / @c inoutputArguments arrays of an AAS operation
         * result), that array *is* the child collection and is returned
         * directly.
         *
         * Returns a pointer to the array stored inside (or being) @p parent's
         * tree; valid as long as @p parent lives.
         */
        const JsArray* childArray(const JsValue& parent)
        {
            // Array-aware: the parent may itself be the child collection.
            if (parent.IsArray())
            {
                return &parent.GetJsArray();
            }

            if (!parent.IsObject()) return nullptr;

            const JsObject& obj = parent.GetJsObject();

            if (auto it = obj.find("submodelElements");
                it != obj.end() && it->second.IsArray())
            {
                return &it->second.GetJsArray();
            }
            if (auto it = obj.find("value"); it != obj.end() && it->second.IsArray())
            {
                return &it->second.GetJsArray();
            }
            return nullptr;
        }

        /**
         * @brief Locates the child of @p parent whose @c idShort equals @p id.
         *
         * Array-aware and OperationVariable-wrapper-aware:
         *  - When @p parent is an array, its elements are scanned directly
         *    (see @ref childArray).
         *  - When @p parent is an object, its @c submodelElements / @c value
         *    child array is scanned (see @ref childArray).
         *  - Each candidate is matched either on its top-level @c idShort or,
         *    for an OperationVariable @c {"value": {...}} wrapper, on the
         *    @c idShort inside its inner @c value object. This mirrors the
         *    "container-duality" idea of @ref childArray, extended to the
         *    @c outputArguments / @c inoutputArguments wrapper.
         *
         * When a matched element is an OperationVariable wrapper (it has no
         * top-level @c idShort but its inner @c value object does), the returned
         * pointer is to that inner @c value object, so that subsequent path
         * segments operate on the real AAS element rather than the wrapper.
         *
         * Returns a pointer into @p parent's tree, or nullptr if @p parent has
         * no child array or none of the children match.
         */
        const JsValue* findElementPtr(const JsValue& parent, std::string_view id)
        {
            const JsArray* children = childArray(parent);
            if (!children) return nullptr;

            for (const JsValue& element : *children)
            {
                if (!element.IsObject()) continue;
                const JsObject& obj = element.GetJsObject();

                // Direct top-level idShort match: return the element as-is.
                if (auto it = obj.find("idShort");
                    it != obj.end() && it->second.IsString() &&
                    it->second.GetString() == id)
                {
                    return &element;
                }

                // OperationVariable wrapper: match against value.idShort and,
                // on success, return the inner value object so that following
                // segments see the real element.
                if (auto v = obj.find("value");
                    v != obj.end() && v->second.IsObject())
                {
                    const JsObject& inner = v->second.GetJsObject();
                    if (auto it = inner.find("idShort");
                        it != inner.end() && it->second.IsString() &&
                        it->second.GetString() == id)
                    {
                        return &v->second;
                    }
                }
            }
            return nullptr;
        }

        /**
         * @brief Parses @p str as a double, tolerating only trailing ASCII
         *        whitespace. Returns std::nullopt on any other trailing text or
         *        a parse failure.
         */
        std::optional<double> parseStrictDouble(const std::string& str)
        {
            try
            {
                std::size_t consumed = 0;
                const double v = std::stod(str, &consumed);
                while (consumed < str.size())
                {
                    if (!std::isspace(static_cast<unsigned char>(str[consumed])))
                        return std::nullopt;
                    ++consumed;
                }
                return v;
            }
            catch (...)
            {
                return std::nullopt;
            }
        }
    }

    Path Path::parse(std::string_view expr)
    {
        Path path;

        std::size_t pos = 0;
        while (pos <= expr.size())
        {
            // Extract the next '/'-delimited segment.
            std::size_t slash = expr.find('/', pos);
            std::string_view seg =
                (slash == std::string_view::npos)
                    ? expr.substr(pos)
                    : expr.substr(pos, slash - pos);
            pos = (slash == std::string_view::npos) ? expr.size() + 1 : slash + 1;

            if (seg.empty()) continue;

            // "@X" shorthand -> Element(X).
            if (seg.front() == '@')
            {
                path.element(std::string(seg.substr(1)));
                continue;
            }

            // "name[...]" -> Key(name) then Index(n) or Element(idShort=X).
            const std::size_t open = seg.find('[');
            if (open == std::string_view::npos)
            {
                path.key(std::string(seg));
                continue;
            }

            const std::size_t close = seg.find(']', open);
            const std::string_view name = seg.substr(0, open);
            if (!name.empty())
            {
                path.key(std::string(name));
            }

            if (close == std::string_view::npos || close <= open + 1)
            {
                // Malformed bracket; the Key (if any) has been recorded, skip
                // the bracket content defensively.
                continue;
            }

            std::string_view inside = seg.substr(open + 1, close - open - 1);

            // "idShort=X" -> Element(X).
            const std::size_t eq = inside.find('=');
            if (eq != std::string_view::npos)
            {
                std::string_view key_name = inside.substr(0, eq);
                std::string_view value    = inside.substr(eq + 1);
                // Only "idShort" is supported as a predicate key.
                if (key_name == "idShort")
                {
                    path.element(std::string(value));
                }
                continue;
            }

            // "[n]" -> Index(n). Parse defensively; skip on failure.
            bool all_digits = !inside.empty();
            for (char c : inside)
            {
                if (!std::isdigit(static_cast<unsigned char>(c)))
                {
                    all_digits = false;
                    break;
                }
            }
            if (all_digits)
            {
                try
                {
                    const std::size_t idx = static_cast<std::size_t>(std::stoull(std::string(inside)));
                    path.index(idx);
                }
                catch (...)
                {
                    // skip malformed index
                }
            }
        }

        return path;
    }

    Path& Path::key(std::string name)
    {
        segments_.push_back(Segment{Segment::Kind::Key, std::move(name), 0});
        return *this;
    }

    Path& Path::index(std::size_t i)
    {
        segments_.push_back(Segment{Segment::Kind::Index, std::string{}, i});
        return *this;
    }

    Path& Path::element(std::string idShort)
    {
        segments_.push_back(Segment{Segment::Kind::Element, std::move(idShort), 0});
        return *this;
    }

    const JsValue* at(const JsValue& root, const Path& path) noexcept
    {
        const JsValue* node = &root;

        for (const auto& seg : path.segments())
        {
            if (node == nullptr) return nullptr;

            switch (seg.kind)
            {
                case Segment::Kind::Key:
                {
                    if (!node->IsObject()) return nullptr;
                    const JsObject& obj = node->GetJsObject();
                    auto it = obj.find(seg.name);
                    if (it == obj.end()) return nullptr;
                    node = &it->second;
                    break;
                }
                case Segment::Kind::Index:
                {
                    if (!node->IsArray()) return nullptr;
                    const JsArray& arr = node->GetJsArray();
                    if (seg.index >= arr.size()) return nullptr;
                    node = &arr[seg.index];
                    break;
                }
                case Segment::Kind::Element:
                {
                    node = findElementPtr(*node, seg.name);
                    if (node == nullptr) return nullptr;
                    break;
                }
            }
        }

        return node;
    }

    const JsValue* at(const JsValue& root, std::string_view expr) noexcept
    {
        return at(root, Path::parse(expr));
    }

    std::optional<JsValue> get(const JsValue& root, const Path& path)
    {
        const JsValue* node = at(root, path);
        if (node == nullptr) return std::nullopt;
        return *node;
    }

    std::optional<JsValue> get(const JsValue& root, std::string_view expr)
    {
        return get(root, Path::parse(expr));
    }

    std::optional<std::string> getString(const JsValue& root, const Path& path)
    {
        const JsValue* node = at(root, path);
        if (node == nullptr || !node->IsString()) return std::nullopt;
        return node->GetString();
    }

    std::optional<double> getDouble(const JsValue& root, const Path& path)
    {
        const JsValue* node = at(root, path);
        if (node == nullptr) return std::nullopt;

        if (isNumber(*node))
        {
            return asDouble(*node);
        }
        if (node->IsString())
        {
            return parseStrictDouble(node->GetString());
        }
        return std::nullopt;
    }

    std::optional<JsValue> findElementByIdShort(const JsValue& parent,
                                                std::string_view idShort)
    {
        const JsValue* node = findElementPtr(parent, idShort);
        if (node == nullptr) return std::nullopt;
        return *node;
    }

    std::optional<std::string> getElementString(const JsValue& parent,
                                                 std::string_view idShort)
    {
        const JsValue* elem = findElementPtr(parent, idShort);
        if (elem == nullptr || !elem->IsObject()) return std::nullopt;

        const JsObject& obj = elem->GetJsObject();
        auto it = obj.find("value");
        if (it == obj.end() || !it->second.IsString()) return std::nullopt;
        return it->second.GetString();
    }

    std::optional<double> getElementDouble(const JsValue& parent,
                                           std::string_view idShort)
    {
        const JsValue* elem = findElementPtr(parent, idShort);
        if (elem == nullptr || !elem->IsObject()) return std::nullopt;

        const JsObject& obj = elem->GetJsObject();
        auto it = obj.find("value");
        if (it == obj.end()) return std::nullopt;

        if (isNumber(it->second)) return asDouble(it->second);
        if (it->second.IsString()) return parseStrictDouble(it->second.GetString());
        return std::nullopt;
    }

    std::vector<std::string> keyValues(const JsValue& node)
    {
        std::vector<std::string> values;
        if (!node.IsObject()) return values;

        const JsObject& obj = node.GetJsObject();
        auto keys_it = obj.find("keys");
        if (keys_it == obj.end() || !keys_it->second.IsArray()) return values;

        for (const JsValue& key : keys_it->second.GetJsArray())
        {
            if (!key.IsObject()) continue;
            const JsObject& key_obj = key.GetJsObject();
            auto v = key_obj.find("value");
            if (v == key_obj.end() || !v->second.IsString()) continue;
            values.push_back(v->second.GetString());
        }
        return values;
    }

    std::optional<std::string> submodelIdFromRef(const JsValue& ref)
    {
        if (!ref.IsObject()) return std::nullopt;

        const JsObject& obj = ref.GetJsObject();
        auto keys_it = obj.find("keys");
        if (keys_it == obj.end() || !keys_it->second.IsArray()) return std::nullopt;

        std::optional<std::string> last_string_value;
        for (const JsValue& key : keys_it->second.GetJsArray())
        {
            if (!key.IsObject()) continue;
            const JsObject& key_obj = key.GetJsObject();
            auto v = key_obj.find("value");
            if (v == key_obj.end() || !v->second.IsString()) continue;

            const std::string value = v->second.GetString();
            last_string_value = value;

            auto t = key_obj.find("type");
            if (t != key_obj.end() && t->second.IsString() &&
                t->second.GetString() == "Submodel")
            {
                return value;
            }
        }
        return last_string_value;
    }

    bool hasSemanticId(const JsValue& node, std::string_view semanticId)
    {
        if (!node.IsObject()) return false;

        const JsObject& obj = node.GetJsObject();
        auto sem_it = obj.find("semanticId");
        if (sem_it == obj.end() || !sem_it->second.IsObject()) return false;

        for (const auto& value : keyValues(sem_it->second))
        {
            if (value == semanticId) return true;
        }
        return false;
    }

    std::optional<std::string> kind(const JsValue& node)
    {
        if (!node.IsObject()) return std::nullopt;

        const JsObject& obj = node.GetJsObject();
        auto it = obj.find("kind");
        if (it == obj.end() || !it->second.IsString()) return std::nullopt;
        return it->second.GetString();
    }
}
