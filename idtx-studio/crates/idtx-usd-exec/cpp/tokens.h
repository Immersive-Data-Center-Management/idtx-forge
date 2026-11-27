//
// Copyright 2016 Pixar
//
// Licensed under the terms set forth in the LICENSE.txt file available at
// https://openusd.org/license.
//
#ifndef IDTX_TOKENS_H
#define IDTX_TOKENS_H

/// \file IDTX/tokens.h

// XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX
// 
// This is an automatically generated file (by usdGenSchema.py).
// Do not hand-edit!
// 
// XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX

#include "pxr/pxr.h"
#include "./api.h"
#include "pxr/base/tf/staticData.h"
#include "pxr/base/tf/token.h"
#include <vector>

PXR_NAMESPACE_OPEN_SCOPE


/// \class IDTXTokensType
///
/// \link IDTXTokens \endlink provides static, efficient
/// \link TfToken TfTokens\endlink for use in all public USD API.
///
/// These tokens are auto-generated from the module's schema, representing
/// property names, for when you need to fetch an attribute or relationship
/// directly by name, e.g. UsdPrim::GetAttribute(), in the most efficient
/// manner, and allow the compiler to verify that you spelled the name
/// correctly.
///
/// IDTXTokens also contains all of the \em allowedTokens values
/// declared for schema builtin attributes of 'token' scene description type.
/// Use IDTXTokens like so:
///
/// \code
///     gprim.GetMyTokenValuedAttr().Set(IDTXTokens->authorization);
/// \endcode
struct IDTXTokensType {
    IDTX_API IDTXTokensType();
    /// \brief "authorization"
    /// 
    /// IDTXRestDatasource
    const TfToken authorization;
    /// \brief "double"
    /// 
    /// Possible value for IDTXCompute_ValueFromJson::GetJsonValueTypeAttr()
    const TfToken double_;
    /// \brief "double_value"
    /// 
    /// IDTXComputeValueSink
    const TfToken double_value;
    /// \brief "endpoint"
    /// 
    /// IDTXRestDatasource
    const TfToken endpoint;
    /// \brief "float"
    /// 
    /// Possible value for IDTXCompute_ValueFromJson::GetJsonValueTypeAttr()
    const TfToken float_;
    /// \brief "float_value"
    /// 
    /// IDTXComputeValueSink
    const TfToken float_value;
    /// \brief "GET"
    /// 
    /// Fallback value for IDTXRestDatasource::GetMethodAttr()
    const TfToken GET;
    /// \brief "inputs:jsonData"
    /// 
    /// IDTXCompute_ValueFromJson
    const TfToken inputsJsonData;
    /// \brief "inputs:key"
    /// 
    /// IDTXCompute_Environment
    const TfToken inputsKey;
    /// \brief "inputs:template"
    /// 
    /// IDTXCompute_VarReplacements
    const TfToken inputsTemplate;
    /// \brief "interval"
    /// 
    /// IDTXMockDatasource_RandomFloat, IDTXRestDatasource
    const TfToken interval;
    /// \brief "jsonBody"
    /// 
    /// IDTXRestDatasource
    const TfToken jsonBody;
    /// \brief "jsonPath"
    /// 
    /// IDTXCompute_ValueFromJson
    const TfToken jsonPath;
    /// \brief "jsonValueType"
    /// 
    /// IDTXCompute_ValueFromJson
    const TfToken jsonValueType;
    /// \brief "method"
    /// 
    /// IDTXRestDatasource
    const TfToken method;
    /// \brief "none"
    /// 
    /// Possible value for IDTXCompute_ValueFromJson::GetJsonValueTypeAttr()
    const TfToken none;
    /// \brief "outputs:data"
    /// 
    /// IDTXDatasource
    const TfToken outputsData;
    /// \brief "outputs:jsonValue:double"
    /// 
    /// IDTXCompute_ValueFromJson
    const TfToken outputsJsonValueDouble;
    /// \brief "outputs:jsonValue:float"
    /// 
    /// IDTXCompute_ValueFromJson
    const TfToken outputsJsonValueFloat;
    /// \brief "outputs:jsonValue:string"
    /// 
    /// IDTXCompute_ValueFromJson
    const TfToken outputsJsonValueString;
    /// \brief "outputs:result"
    /// 
    /// IDTXCompute_VarReplacements
    const TfToken outputsResult;
    /// \brief "outputs:value"
    /// 
    /// IDTXCompute_Environment
    const TfToken outputsValue;
    /// \brief "POST"
    /// 
    /// Possible value for IDTXRestDatasource::GetMethodAttr()
    const TfToken POST;
    /// \brief "query"
    /// 
    /// IDTXRestDatasource
    const TfToken query;
    /// \brief "string"
    /// 
    /// Possible value for IDTXCompute_ValueFromJson::GetJsonValueTypeAttr()
    const TfToken string;
    /// \brief "string_value"
    /// 
    /// IDTXComputeValueSink
    const TfToken string_value;
    /// \brief "Compute_Environment"
    /// 
    /// Schema identifer and family for IDTXCompute_Environment
    const TfToken Compute_Environment;
    /// \brief "Compute_ValueFromAASJson"
    ///
    /// Schema identifer and family for IDTXCompute_ValueFromAASJson
    const TfToken Compute_ValueFromAASJson;
    /// \brief "Compute_ValueFromJson"
    ///
    /// Schema identifer and family for IDTXCompute_ValueFromJson
    const TfToken Compute_ValueFromJson;
    /// \brief "Compute_VarReplacements"
    /// 
    /// Schema identifer and family for IDTXCompute_VarReplacements
    const TfToken Compute_VarReplacements;
    /// \brief "ComputeValueSink"
    /// 
    /// Schema identifer and family for IDTXComputeValueSink
    const TfToken ComputeValueSink;
    /// \brief "Datasource"
    /// 
    /// Schema identifer and family for IDTXDatasource
    const TfToken Datasource;
    /// \brief "MockDatasource_RandomFloat"
    /// 
    /// Schema identifer and family for IDTXMockDatasource_RandomFloat
    const TfToken MockDatasource_RandomFloat;
    /// \brief "RestDatasource"
    /// 
    /// Schema identifer and family for IDTXRestDatasource
    const TfToken RestDatasource;
    /// A vector of all of the tokens listed above.
    const std::vector<TfToken> allTokens;
};

/// \var IDTXTokens
///
/// A global variable with static, efficient \link TfToken TfTokens\endlink
/// for use in all public USD API.  \sa IDTXTokensType
extern IDTX_API TfStaticData<IDTXTokensType> IDTXTokens;

PXR_NAMESPACE_CLOSE_SCOPE

#endif
