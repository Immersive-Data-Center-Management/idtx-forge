//
// Copyright 2016 Pixar
//
// Licensed under the terms set forth in the LICENSE.txt file available at
// https://openusd.org/license.
//
#include "./compute_ValueFromAASJson.h"
#include "pxr/usd/usd/schemaRegistry.h"
#include "pxr/usd/usd/typed.h"

#include "pxr/usd/sdf/types.h"
#include "pxr/usd/sdf/assetPath.h"

PXR_NAMESPACE_OPEN_SCOPE

// Register the schema with the TfType system.
TF_REGISTRY_FUNCTION(TfType)
{
    TfType::Define<IDTXCompute_ValueFromAASJson,
        TfType::Bases< UsdTyped > >();

    // Register the usd prim typename as an alias under UsdSchemaBase. This
    // enables one to call
    // TfType::Find<UsdSchemaBase>().FindDerivedByName("Compute_ValueFromAASJson")
    // to find TfType<IDTXCompute_ValueFromAASJson>, which is how IsA queries are
    // answered.
    TfType::AddAlias<UsdSchemaBase, IDTXCompute_ValueFromAASJson>("Compute_ValueFromAASJson");
}

/* virtual */
IDTXCompute_ValueFromAASJson::~IDTXCompute_ValueFromAASJson()
{
}

/* static */
IDTXCompute_ValueFromAASJson
IDTXCompute_ValueFromAASJson::Get(const UsdStagePtr &stage, const SdfPath &path)
{
    if (!stage) {
        TF_CODING_ERROR("Invalid stage");
        return IDTXCompute_ValueFromAASJson();
    }
    return IDTXCompute_ValueFromAASJson(stage->GetPrimAtPath(path));
}

/* static */
IDTXCompute_ValueFromAASJson
IDTXCompute_ValueFromAASJson::Define(
    const UsdStagePtr &stage, const SdfPath &path)
{
    static TfToken usdPrimTypeName("Compute_ValueFromAASJson");
    if (!stage) {
        TF_CODING_ERROR("Invalid stage");
        return IDTXCompute_ValueFromAASJson();
    }
    return IDTXCompute_ValueFromAASJson(
        stage->DefinePrim(path, usdPrimTypeName));
}

/* virtual */
UsdSchemaKind IDTXCompute_ValueFromAASJson::_GetSchemaKind() const
{
    return IDTXCompute_ValueFromAASJson::schemaKind;
}

/* static */
const TfType &
IDTXCompute_ValueFromAASJson::_GetStaticTfType()
{
    static TfType tfType = TfType::Find<IDTXCompute_ValueFromAASJson>();
    return tfType;
}

/* static */
bool
IDTXCompute_ValueFromAASJson::_IsTypedSchema()
{
    static bool isTyped = _GetStaticTfType().IsA<UsdTyped>();
    return isTyped;
}

/* virtual */
const TfType &
IDTXCompute_ValueFromAASJson::_GetTfType() const
{
    return _GetStaticTfType();
}

UsdAttribute
IDTXCompute_ValueFromAASJson::GetInputsJsonDataAttr() const
{
    return GetPrim().GetAttribute(IDTXTokens->inputsJsonData);
}

UsdAttribute
IDTXCompute_ValueFromAASJson::CreateInputsJsonDataAttr(VtValue const &defaultValue, bool writeSparsely) const
{
    return UsdSchemaBase::_CreateAttr(IDTXTokens->inputsJsonData,
                       SdfValueTypeNames->String,
                       /* custom = */ false,
                       SdfVariabilityVarying,
                       defaultValue,
                       writeSparsely);
}

UsdAttribute
IDTXCompute_ValueFromAASJson::GetJsonPathAttr() const
{
    return GetPrim().GetAttribute(IDTXTokens->jsonPath);
}

UsdAttribute
IDTXCompute_ValueFromAASJson::CreateJsonPathAttr(VtValue const &defaultValue, bool writeSparsely) const
{
    return UsdSchemaBase::_CreateAttr(IDTXTokens->jsonPath,
                       SdfValueTypeNames->String,
                       /* custom = */ false,
                       SdfVariabilityVarying,
                       defaultValue,
                       writeSparsely);
}

UsdAttribute
IDTXCompute_ValueFromAASJson::GetJsonValueTypeAttr() const
{
    return GetPrim().GetAttribute(IDTXTokens->jsonValueType);
}

UsdAttribute
IDTXCompute_ValueFromAASJson::CreateJsonValueTypeAttr(VtValue const &defaultValue, bool writeSparsely) const
{
    return UsdSchemaBase::_CreateAttr(IDTXTokens->jsonValueType,
                       SdfValueTypeNames->Token,
                       /* custom = */ false,
                       SdfVariabilityVarying,
                       defaultValue,
                       writeSparsely);
}

UsdAttribute
IDTXCompute_ValueFromAASJson::GetOutputsJsonValueDoubleAttr() const
{
    return GetPrim().GetAttribute(IDTXTokens->outputsJsonValueDouble);
}

UsdAttribute
IDTXCompute_ValueFromAASJson::CreateOutputsJsonValueDoubleAttr(VtValue const &defaultValue, bool writeSparsely) const
{
    return UsdSchemaBase::_CreateAttr(IDTXTokens->outputsJsonValueDouble,
                       SdfValueTypeNames->Double,
                       /* custom = */ false,
                       SdfVariabilityVarying,
                       defaultValue,
                       writeSparsely);
}

UsdAttribute
IDTXCompute_ValueFromAASJson::GetOutputsJsonValueFloatAttr() const
{
    return GetPrim().GetAttribute(IDTXTokens->outputsJsonValueFloat);
}

UsdAttribute
IDTXCompute_ValueFromAASJson::CreateOutputsJsonValueFloatAttr(VtValue const &defaultValue, bool writeSparsely) const
{
    return UsdSchemaBase::_CreateAttr(IDTXTokens->outputsJsonValueFloat,
                       SdfValueTypeNames->Float,
                       /* custom = */ false,
                       SdfVariabilityVarying,
                       defaultValue,
                       writeSparsely);
}

UsdAttribute
IDTXCompute_ValueFromAASJson::GetOutputsJsonValueStringAttr() const
{
    return GetPrim().GetAttribute(IDTXTokens->outputsJsonValueString);
}

UsdAttribute
IDTXCompute_ValueFromAASJson::CreateOutputsJsonValueStringAttr(VtValue const &defaultValue, bool writeSparsely) const
{
    return UsdSchemaBase::_CreateAttr(IDTXTokens->outputsJsonValueString,
                       SdfValueTypeNames->String,
                       /* custom = */ false,
                       SdfVariabilityVarying,
                       defaultValue,
                       writeSparsely);
}

namespace {
static inline TfTokenVector
_ConcatenateAttributeNames(const TfTokenVector& left,const TfTokenVector& right)
{
    TfTokenVector result;
    result.reserve(left.size() + right.size());
    result.insert(result.end(), left.begin(), left.end());
    result.insert(result.end(), right.begin(), right.end());
    return result;
}
}

/*static*/
const TfTokenVector&
IDTXCompute_ValueFromAASJson::GetSchemaAttributeNames(bool includeInherited)
{
    static TfTokenVector localNames = {
        IDTXTokens->inputsJsonData,
        IDTXTokens->jsonPath,
        IDTXTokens->jsonValueType,
        IDTXTokens->outputsJsonValueDouble,
        IDTXTokens->outputsJsonValueFloat,
        IDTXTokens->outputsJsonValueString,
    };
    static TfTokenVector allNames =
        _ConcatenateAttributeNames(
            UsdTyped::GetSchemaAttributeNames(true),
            localNames);

    if (includeInherited)
        return allNames;
    else
        return localNames;
}

PXR_NAMESPACE_CLOSE_SCOPE

// ===================================================================== //
// Feel free to add custom code below this line. It will be preserved by
// the code generator.
//
// Just remember to wrap code in the appropriate delimiters:
// 'PXR_NAMESPACE_OPEN_SCOPE', 'PXR_NAMESPACE_CLOSE_SCOPE'.
// ===================================================================== //
// --(BEGIN CUSTOM CODE)--
