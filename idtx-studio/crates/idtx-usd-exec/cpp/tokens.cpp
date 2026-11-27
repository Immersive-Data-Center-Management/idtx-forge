//
// Copyright 2016 Pixar
//
// Licensed under the terms set forth in the LICENSE.txt file available at
// https://openusd.org/license.
//
#include "./tokens.h"

PXR_NAMESPACE_OPEN_SCOPE

IDTXTokensType::IDTXTokensType() :
    authorization("authorization", TfToken::Immortal),
    double_("double", TfToken::Immortal),
    double_value("double_value", TfToken::Immortal),
    endpoint("endpoint", TfToken::Immortal),
    float_("float", TfToken::Immortal),
    float_value("float_value", TfToken::Immortal),
    GET("GET", TfToken::Immortal),
    inputsJsonData("inputs:jsonData", TfToken::Immortal),
    inputsKey("inputs:key", TfToken::Immortal),
    inputsTemplate("inputs:template", TfToken::Immortal),
    interval("interval", TfToken::Immortal),
    jsonBody("jsonBody", TfToken::Immortal),
    jsonPath("jsonPath", TfToken::Immortal),
    jsonValueType("jsonValueType", TfToken::Immortal),
    method("method", TfToken::Immortal),
    none("none", TfToken::Immortal),
    outputsData("outputs:data", TfToken::Immortal),
    outputsJsonValueDouble("outputs:jsonValue:double", TfToken::Immortal),
    outputsJsonValueFloat("outputs:jsonValue:float", TfToken::Immortal),
    outputsJsonValueString("outputs:jsonValue:string", TfToken::Immortal),
    outputsResult("outputs:result", TfToken::Immortal),
    outputsValue("outputs:value", TfToken::Immortal),
    POST("POST", TfToken::Immortal),
    query("query", TfToken::Immortal),
    string("string", TfToken::Immortal),
    string_value("string_value", TfToken::Immortal),
    Compute_Environment("Compute_Environment", TfToken::Immortal),
    Compute_ValueFromAASJson("Compute_ValueFromAASJson", TfToken::Immortal),
    Compute_ValueFromJson("Compute_ValueFromJson", TfToken::Immortal),
    Compute_VarReplacements("Compute_VarReplacements", TfToken::Immortal),
    ComputeValueSink("ComputeValueSink", TfToken::Immortal),
    Datasource("Datasource", TfToken::Immortal),
    MockDatasource_RandomFloat("MockDatasource_RandomFloat", TfToken::Immortal),
    RestDatasource("RestDatasource", TfToken::Immortal),
    allTokens({
        authorization,
        double_,
        double_value,
        endpoint,
        float_,
        float_value,
        GET,
        inputsJsonData,
        inputsKey,
        inputsTemplate,
        interval,
        jsonBody,
        jsonPath,
        jsonValueType,
        method,
        none,
        outputsData,
        outputsJsonValueDouble,
        outputsJsonValueFloat,
        outputsJsonValueString,
        outputsResult,
        outputsValue,
        POST,
        query,
        string,
        string_value,
        Compute_Environment,
        Compute_ValueFromAASJson,
        Compute_ValueFromJson,
        Compute_VarReplacements,
        ComputeValueSink,
        Datasource,
        MockDatasource_RandomFloat,
        RestDatasource
    })
{
}

TfStaticData<IDTXTokensType> IDTXTokens;

PXR_NAMESPACE_CLOSE_SCOPE
