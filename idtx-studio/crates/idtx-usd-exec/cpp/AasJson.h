#pragma once

/**
 * @file AasJson.h
 * @brief Reusable, unit-tested accessor utility for reading values out of AAS
 *        (Asset Administration Shell) JSON responses.
 *
 * Vendored from the sibling `idtx-flow-idcm` project (usd/source/AasJson.{h,cpp})
 * so the IDTX OpenExec plugin can host `Compute_ValueFromAASJson` alongside the
 * RFC-6901 `Compute_ValueFromJson`. Self-contained and prefix-agnostic: it uses
 * only OpenUSD's `pxr/base/js` JSON value types, no third-party JSON dependency.
 *
 * Parsing of AAS JSON was historically scattered across several translation
 * units, each re-implementing the same defensive @c IsObject() / @c IsArray()
 * / @c GetString() walk by hand. This module centralises that logic
 * behind a single, @c std::optional -returning API so that callers express
 * *what* they want to read, not *how* to walk the tree.
 *
 * Two addressing dimensions are supported:
 *
 *  1. **Structural JSON addressing** — object keys and array indices, for
 *     properties like @c result, @c paging_metadata.cursor,
 *     @c semanticId.keys[0].value, @c access_token and @c value.keys[0].value.
 *  2. **AAS-semantic addressing by @c idShort** — submodel-element lookups that
 *     transparently descend through @c submodelElements *and* @c value
 *     containers (the SubmodelElementCollection quirk). This is the dominant
 *     AAS access pattern and cannot be expressed with a plain JSON Pointer.
 *
 * A @ref Path is an ordered list of typed @ref Segment values. Paths can be
 * built programmatically (type-safe) or parsed from a compact, readable string
 * form:
 *
 *  - @c "result[0]/keys[0]/value"                — structural.
 *  - @c "paging_metadata/cursor"                 — structural.
 *  - @c "submodelElements[idShort=USDFile]/value"— idShort search + value.
 *  - @c "@USDFile/value"                         — shorthand @c \@name == Element(name).
 *
 * None of the accessors throw on a missing or wrongly-typed node; they return
 * an empty optional (or @c nullptr for the raw pointer overload) instead. This
 * matches the surrounding optional-based style of @c AasService.
 *
 * This module uses OpenUSD's JSON value types (@c JsValue, @c JsObject,
 * @c JsArray) from @c pxr/base/js instead of a third-party JSON library.
 *
 */

#include <optional>
#include <string>
#include <string_view>
#include <vector>

#include <pxr/base/js/value.h>
#include <pxr/base/js/types.h>

namespace idtx::aas_json
{
    /** @brief Convenience alias for the OpenUSD JSON value type. */
    using Json = PXR_NS::JsValue;

    /**
     * @brief A single addressing step within a @ref Path.
     *
     * Exactly one of the three kinds is active per segment:
     *  - @c Key     — descend into an object member named @ref name.
     *  - @c Index   — descend into the array element at @ref index.
     *  - @c Element — descend into the AAS child (under @c submodelElements or
     *                 @c value) whose @c idShort equals @ref name.
     */
    struct Segment
    {
        /** @brief Discriminator selecting which field of the segment is used. */
        enum class Kind
        {
            Key,     ///< Object member lookup by @ref name.
            Index,   ///< Array element lookup by @ref index.
            Element  ///< AAS child lookup by @c idShort == @ref name.
        };

        Kind        kind;         ///< Active segment kind.
        std::string name;         ///< Member name (Key) or idShort (Element).
        std::size_t index = 0;    ///< Array index (Index).
    };

    /**
     * @brief An ordered sequence of @ref Segment values addressing a node in an
     *        AAS JSON tree.
     *
     * A Path can be assembled programmatically via the fluent @ref key /
     * @ref index / @ref element builders, or parsed from a compact string with
     * @ref parse. The two forms are equivalent; the string form is provided for
     * ergonomic call sites while the builders provide a type-safe alternative
     * that cannot suffer from string typos.
     */
    class Path
    {
    public:
        /**
         * @brief Parses a compact path expression into a @ref Path.
         *
         * Grammar (intentionally tiny):
         *  - Segments are separated by @c '/'. Empty segments are ignored.
         *  - @c name            → @c Key(name).
         *  - @c name[n]         → @c Key(name) then @c Index(n).
         *  - @c name[idShort=X] → @c Key(name) then @c Element(X).
         *  - @c \@X             → @c Element(X) (idShort search under
         *                          @c submodelElements / @c value).
         *
         * Malformed segments are skipped defensively rather than throwing, so a
         * typo yields a Path that simply fails to resolve (returning an empty
         * optional at read time) instead of aborting the program.
         *
         * @param expr  The path expression, e.g. @c "result[0]/keys[0]/value".
         * @return      The parsed Path.
         */
        static Path parse(std::string_view expr);

        /**
         * @brief Appends a @c Key segment.
         * @param name  Object member name to descend into.
         * @return      @c *this, for fluent chaining.
         */
        Path& key(std::string name);

        /**
         * @brief Appends an @c Index segment.
         * @param i  Zero-based array index to descend into.
         * @return   @c *this, for fluent chaining.
         */
        Path& index(std::size_t i);

        /**
         * @brief Appends an @c Element segment (AAS idShort search).
         * @param idShort  The @c idShort of the child element to descend into.
         * @return         @c *this, for fluent chaining.
         */
        Path& element(std::string idShort);

        /** @brief Returns the ordered segments of this path. */
        const std::vector<Segment>& segments() const noexcept { return segments_; }

        /** @brief Returns true if the path has no segments. */
        bool empty() const noexcept { return segments_.empty(); }

    private:
        std::vector<Segment> segments_;
    };

    /**
     * @brief Navigates @p root following @p path and returns a pointer to the
     *        addressed node, or @c nullptr if any segment fails to resolve.
     *
     * Navigation semantics:
     *  - @c Key     — object member lookup; a miss stops navigation.
     *  - @c Index   — array element lookup; out-of-range stops navigation.
     *  - @c Element — locates the child array (prefer @c submodelElements, else
     *                 @c value) and linear-scans for @c idShort == name.
     *
     * Never throws.
     *
     * @param root  The root JSON node to navigate from.
     * @param path  The path to follow.
     * @return      Pointer into @p root's tree, or @c nullptr on any miss.
     */
    const PXR_NS::JsValue* at(const PXR_NS::JsValue& root, const Path& path) noexcept;

    /**
     * @brief Convenience overload accepting a compact path expression.
     * @param root  The root JSON node to navigate from.
     * @param expr  The path expression (see @ref Path::parse).
     * @return      Pointer into @p root's tree, or @c nullptr on any miss.
     */
    const PXR_NS::JsValue* at(const PXR_NS::JsValue& root, std::string_view expr) noexcept;

    /**
     * @brief Returns a copy of the node addressed by @p path, or std::nullopt.
     * @param root  The root JSON node.
     * @param path  The path to follow.
     * @return      The addressed node (copied), or std::nullopt on a miss.
     */
    std::optional<PXR_NS::JsValue> get(const PXR_NS::JsValue& root, const Path& path);

    /**
     * @brief Convenience overload accepting a compact path expression.
     * @param root  The root JSON node.
     * @param expr  The path expression (see @ref Path::parse).
     * @return      The addressed node (copied), or std::nullopt on a miss.
     */
    std::optional<PXR_NS::JsValue> get(const PXR_NS::JsValue& root, std::string_view expr);

    /**
     * @brief Reads the string value at @p path.
     * @param root  The root JSON node.
     * @param path  The path to follow.
     * @return      The string, or std::nullopt if missing / not a string.
     */
    std::optional<std::string> getString(const PXR_NS::JsValue& root, const Path& path);

    /**
     * @brief Reads the numeric value at @p path as a @c double.
     *
     * Accepts either a JSON number or a JSON string that parses cleanly as a
     * double via @c std::stod (only trailing ASCII whitespace is tolerated;
     * @c "12abc" is rejected, @c "1.0 " is accepted). This mirrors the strict
     * parsing previously implemented in
     * @c AasPositional::readFloatPropertyByIdShort().
     *
     * @param root  The root JSON node.
     * @param path  The path to follow.
     * @return      The parsed double, or std::nullopt on a miss / parse failure.
     */
    std::optional<double> getDouble(const PXR_NS::JsValue& root, const Path& path);

    /**
     * @brief Locates the AAS child of @p parent whose @c idShort equals
     *        @p idShort, descending through @c submodelElements or @c value.
     *
     * This is the single canonical implementation of the "find child by
     * idShort" behaviour that was previously duplicated across the codebase.
     *
     * @param parent   A Submodel or SubmodelElementCollection JSON object.
     * @param idShort  The idShort to match (exact, case sensitive).
     * @return         The matching element (copied), or std::nullopt.
     */
    std::optional<PXR_NS::JsValue> findElementByIdShort(const PXR_NS::JsValue& parent,
                                                        std::string_view idShort);

    /**
     * @brief Reads the string @c value of the child of @p parent whose
     *        @c idShort equals @p idShort.
     *
     * @param parent   A Submodel or SubmodelElementCollection JSON object.
     * @param idShort  The idShort of the Property child.
     * @return         The string value, or std::nullopt.
     */
    std::optional<std::string> getElementString(const PXR_NS::JsValue& parent,
                                                 std::string_view idShort);

    /**
     * @brief Reads the @c value of the child of @p parent whose @c idShort
     *        equals @p idShort, parsed as a @c double (see @ref getDouble).
     *
     * @param parent   A Submodel or SubmodelElementCollection JSON object.
     * @param idShort  The idShort of the Property child.
     * @return         The parsed double, or std::nullopt.
     */
    std::optional<double> getElementDouble(const PXR_NS::JsValue& parent,
                                           std::string_view idShort);

    /**
     * @brief Collects the string @c value of every entry of @p node's
     *        @c keys array.
     *
     * Non-object keys and keys without a string @c value are skipped. Used by
     * the suffix-matching callers (@c fetchSubmodelId, @c findShellsWithSubmodel)
     * that need to inspect all reference keys.
     *
     * @param node  A reference-shaped JSON object carrying a @c keys array.
     * @return      The list of string key values (possibly empty).
     */
    std::vector<std::string> keyValues(const PXR_NS::JsValue& node);

    /**
     * @brief Extracts the submodel id from a submodel-ref JSON entry.
     *
     * Picks the @c value of the first key whose @c type == @c "Submodel";
     * falls back to the last key that carries a string @c value. This preserves
     * the exact semantics of the former @c extractSubmodelIdFromRef() helper
     * (NOT the "first match" rule of the former @c fetchSubmodelId()).
     *
     * @param ref  A submodel-ref JSON object carrying a @c keys array.
     * @return     The submodel id, or std::nullopt if no string key exists.
     */
    std::optional<std::string> submodelIdFromRef(const PXR_NS::JsValue& ref);

    /**
     * @brief Returns true if @p node has a @c semanticId whose any
     *        @c keys[].value equals @p semanticId (exact, case sensitive).
     *
     * @param node        A submodel (or element) JSON object.
     * @param semanticId  The semantic id to match.
     * @return            True on a match, false otherwise.
     */
    bool hasSemanticId(const PXR_NS::JsValue& node, std::string_view semanticId);

    /**
     * @brief Returns the string kind field of the node, or std::nullopt.
     *
     * Preserves the semantics of the former AasService kindMatches() helper:
     * the value is read verbatim (e.g. "Instance" / "Template"). Callers
     * compare it against the desired kind themselves.
     *
     * @param node  A submodel JSON object.
     * @return      The kind string, or std::nullopt if absent / not a string.
     */
    std::optional<std::string> kind(const PXR_NS::JsValue& node);
}
