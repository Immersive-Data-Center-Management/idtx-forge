//
// Copyright 2016 Pixar
//
// Licensed under the terms set forth in the LICENSE.txt file available at
// https://openusd.org/license.
//
#include "./compute_ValueFromJson.h"
#include "pxr/usd/usd/schemaRegistry.h"
#include "pxr/usd/usd/typed.h"

#include "pxr/usd/sdf/types.h"
#include "pxr/usd/sdf/assetPath.h"

PXR_NAMESPACE_OPEN_SCOPE

// Register the schema with the TfType system.
TF_REGISTRY_FUNCTION(TfType)
{
    TfType::Define<IDTXCompute_ValueFromJson,
        TfType::Bases< UsdTyped > >();
    
    // Register the usd prim typename as an alias under UsdSchemaBase. This
    // enables one to call
    // TfType::Find<UsdSchemaBase>().FindDerivedByName("Compute_ValueFromJson")
    // to find TfType<IDTXCompute_ValueFromJson>, which is how IsA queries are
    // answered.
    TfType::AddAlias<UsdSchemaBase, IDTXCompute_ValueFromJson>("Compute_ValueFromJson");
}

/* virtual */
IDTXCompute_ValueFromJson::~IDTXCompute_ValueFromJson()
{
}

/* static */
IDTXCompute_ValueFromJson
IDTXCompute_ValueFromJson::Get(const UsdStagePtr &stage, const SdfPath &path)
{
    if (!stage) {
        TF_CODING_ERROR("Invalid stage");
        return IDTXCompute_ValueFromJson();
    }
    return IDTXCompute_ValueFromJson(stage->GetPrimAtPath(path));
}

/* static */
IDTXCompute_ValueFromJson
IDTXCompute_ValueFromJson::Define(
    const UsdStagePtr &stage, const SdfPath &path)
{
    static TfToken usdPrimTypeName("Compute_ValueFromJson");
    if (!stage) {
        TF_CODING_ERROR("Invalid stage");
        return IDTXCompute_ValueFromJson();
    }
    return IDTXCompute_ValueFromJson(
        stage->DefinePrim(path, usdPrimTypeName));
}

/* virtual */
UsdSchemaKind IDTXCompute_ValueFromJson::_GetSchemaKind() const
{
    return IDTXCompute_ValueFromJson::schemaKind;
}

/* static */
const TfType &
IDTXCompute_ValueFromJson::_GetStaticTfType()
{
    static TfType tfType = TfType::Find<IDTXCompute_ValueFromJson>();
    return tfType;
}

/* static */
bool 
IDTXCompute_ValueFromJson::_IsTypedSchema()
{
    static bool isTyped = _GetStaticTfType().IsA<UsdTyped>();
    return isTyped;
}

/* virtual */
const TfType &
IDTXCompute_ValueFromJson::_GetTfType() const
{
    return _GetStaticTfType();
}

UsdAttribute
IDTXCompute_ValueFromJson::GetInputsJsonDataAttr() const
{
    return GetPrim().GetAttribute(IDTXTokens->inputsJsonData);
}

UsdAttribute
IDTXCompute_ValueFromJson::CreateInputsJsonDataAttr(VtValue const &defaultValue, bool writeSparsely) const
{
    return UsdSchemaBase::_CreateAttr(IDTXTokens->inputsJsonData,
                       SdfValueTypeNames->String,
                       /* custom = */ false,
                       SdfVariabilityVarying,
                       defaultValue,
                       writeSparsely);
}

UsdAttribute
IDTXCompute_ValueFromJson::GetJsonPathAttr() const
{
    return GetPrim().GetAttribute(IDTXTokens->jsonPath);
}

UsdAttribute
IDTXCompute_ValueFromJson::CreateJsonPathAttr(VtValue const &defaultValue, bool writeSparsely) const
{
    return UsdSchemaBase::_CreateAttr(IDTXTokens->jsonPath,
                       SdfValueTypeNames->String,
                       /* custom = */ false,
                       SdfVariabilityVarying,
                       defaultValue,
                       writeSparsely);
}

UsdAttribute
IDTXCompute_ValueFromJson::GetJsonValueTypeAttr() const
{
    return GetPrim().GetAttribute(IDTXTokens->jsonValueType);
}

UsdAttribute
IDTXCompute_ValueFromJson::CreateJsonValueTypeAttr(VtValue const &defaultValue, bool writeSparsely) const
{
    return UsdSchemaBase::_CreateAttr(IDTXTokens->jsonValueType,
                       SdfValueTypeNames->Token,
                       /* custom = */ false,
                       SdfVariabilityVarying,
                       defaultValue,
                       writeSparsely);
}

UsdAttribute
IDTXCompute_ValueFromJson::GetOutputsJsonValueDoubleAttr() const
{
    return GetPrim().GetAttribute(IDTXTokens->outputsJsonValueDouble);
}

UsdAttribute
IDTXCompute_ValueFromJson::CreateOutputsJsonValueDoubleAttr(VtValue const &defaultValue, bool writeSparsely) const
{
    return UsdSchemaBase::_CreateAttr(IDTXTokens->outputsJsonValueDouble,
                       SdfValueTypeNames->Double,
                       /* custom = */ false,
                       SdfVariabilityVarying,
                       defaultValue,
                       writeSparsely);
}

UsdAttribute
IDTXCompute_ValueFromJson::GetOutputsJsonValueFloatAttr() const
{
    return GetPrim().GetAttribute(IDTXTokens->outputsJsonValueFloat);
}

UsdAttribute
IDTXCompute_ValueFromJson::CreateOutputsJsonValueFloatAttr(VtValue const &defaultValue, bool writeSparsely) const
{
    return UsdSchemaBase::_CreateAttr(IDTXTokens->outputsJsonValueFloat,
                       SdfValueTypeNames->Float,
                       /* custom = */ false,
                       SdfVariabilityVarying,
                       defaultValue,
                       writeSparsely);
}

UsdAttribute
IDTXCompute_ValueFromJson::GetOutputsJsonValueStringAttr() const
{
    return GetPrim().GetAttribute(IDTXTokens->outputsJsonValueString);
}

UsdAttribute
IDTXCompute_ValueFromJson::CreateOutputsJsonValueStringAttr(VtValue const &defaultValue, bool writeSparsely) const
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
IDTXCompute_ValueFromJson::GetSchemaAttributeNames(bool includeInherited)
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
