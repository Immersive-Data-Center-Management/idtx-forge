/**
 * @file computation_valuefromaasjson.cpp
 * @brief Implementation of the computation registered along the
 *        IDTXCompute_ValueFromAASJson schema.
 *
 * Ported from the sibling `idtx-flow-idcm` project
 * (usd/source/computation_valuefromaasjson.cpp), retargeted to the IDTX library
 * prefix (`IDTXTokens`, `IDTXCompute_ValueFromAASJson`). It mirrors the
 * RFC-6901 `Compute_ValueFromJson` computation but resolves the configured
 * `jsonPath` through the AAS idShort-aware accessor in `AasJson.h`, which can
 * descend `submodelElements` / `value` containers and OperationVariable
 * wrappers (`outputArguments[idShort=…]/@Record000/@Field/value`) that a plain
 * JSON Pointer cannot express.
 **/

#include <string>

#include <pxr/base/plug/registry.h>
#include <pxr/exec/exec/registerSchema.h>
#include <pxr/exec/vdf/context.h>
#include <pxr/base/js/json.h>
#include <pxr/base/js/value.h>

#include <tokens.h>
#include <AasJson.h>

PXR_NAMESPACE_USING_DIRECTIVE

TF_DEFINE_PRIVATE_TOKENS(_IDTXAASTokens,
    (resolvedValue)  // Convention: bridges prim computation output → attribute computation
    ((jsonValueFloatBaseName, "float"))
    ((jsonValueDoubleBaseName, "double"))
    ((jsonValueStringBaseName, "string"))
);

EXEC_REGISTER_COMPUTATIONS_FOR_SCHEMA(IDTXCompute_ValueFromAASJson)
{
    self.PrimComputation(IDTXTokens->outputsJsonValueFloat)
        .Callback<float>(+[](const VdfContext &ctx) {
            const std::string* jsonDataPtr = ctx.GetInputValuePtr<std::string>(IDTXTokens->inputsJsonData);
            if (!jsonDataPtr)
            {
                ctx.SetOutput<float>(0.0);
                return;
            }
            const std::string& jsonPath = ctx.GetInputValue<std::string>(IDTXTokens->jsonPath);

            // Parse the JSON string
            JsParseError parseError;
            const JsValue root = JsParseString(*jsonDataPtr, &parseError);
            if (root.IsNull()) {
                ctx.SetOutput<float>(0.0);
                return;
            }

            // Resolve the AAS JSON path into the tree and provide the value
            const auto value = idtx::aas_json::getDouble(root, idtx::aas_json::Path::parse(jsonPath));
            ctx.SetOutput<float>(static_cast<float>(value.value_or(0.0)));
        })
        .Inputs(
            Attribute(IDTXTokens->inputsJsonData)
                .Connections<std::string>(ExecBuiltinComputations->computeValue)
                .InputName(IDTXTokens->inputsJsonData),
            AttributeValue<std::string>(IDTXTokens->jsonPath).Required()
        );

    // Register the computed value as computation to make it accessible via ConnectionTargetedObjects
    self.AttributeComputation(IDTXTokens->outputsJsonValueFloat, _IDTXAASTokens->resolvedValue)
        .Callback<float>(+[](const VdfContext &ctx)
        {
            ctx.SetOutput<float>(
                ctx.GetInputValue<float>(IDTXTokens->outputsJsonValueFloat)
            );
        })
        .Inputs(
            Prim()
                .Computation<float>(IDTXTokens->outputsJsonValueFloat)
        );

    self.PrimComputation(IDTXTokens->outputsJsonValueDouble)
        .Callback<double>(+[](const VdfContext &ctx) {
            const std::string* jsonDataPtr = ctx.GetInputValuePtr<std::string>(IDTXTokens->inputsJsonData);
            if (!jsonDataPtr)
            {
                ctx.SetOutput<double>(0.0);
                return;
            }
            const std::string& jsonPath = ctx.GetInputValue<std::string>(IDTXTokens->jsonPath);

            // Parse the JSON string
            JsParseError parseError;
            const JsValue root = JsParseString(*jsonDataPtr, &parseError);
            if (root.IsNull()) {
                ctx.SetOutput<double>(0.0);
                return;
            }

            // Resolve the AAS JSON path into the tree and provide the value
            const auto value = idtx::aas_json::getDouble(root, idtx::aas_json::Path::parse(jsonPath));
            ctx.SetOutput<double>(value.value_or(0.0));
        })
        .Inputs(
            Attribute(IDTXTokens->inputsJsonData)
                .Connections<std::string>(ExecBuiltinComputations->computeValue)
                .InputName(IDTXTokens->inputsJsonData),
            AttributeValue<std::string>(IDTXTokens->jsonPath)
                .Required()
        );

    // Register the computed value as computation to make it accessible via ConnectionTargetedObjects
    self.AttributeComputation(IDTXTokens->outputsJsonValueDouble, _IDTXAASTokens->resolvedValue)
        .Callback<double>(+[](const VdfContext &ctx)
        {
            ctx.SetOutput<double>(
                ctx.GetInputValue<double>(IDTXTokens->outputsJsonValueDouble)
            );
        })
        .Inputs(
            Prim().Computation<double>(IDTXTokens->outputsJsonValueDouble)
        );

    self.PrimComputation(IDTXTokens->outputsJsonValueString)
        .Callback<std::string>(+[](const VdfContext &ctx) {
            const std::string* jsonDataPtr = ctx.GetInputValuePtr<std::string>(IDTXTokens->inputsJsonData);
            if (!jsonDataPtr)
            {
                ctx.SetOutput<std::string>("");
                return;
            }
            const std::string& jsonPath = ctx.GetInputValue<std::string>(IDTXTokens->jsonPath);

            // Parse the JSON string
            JsParseError parseError;
            const JsValue root = JsParseString(*jsonDataPtr, &parseError);
            if (root.IsNull()) {
                ctx.SetOutput<std::string>("");
                return;
            }

            // Resolve the AAS JSON path into the tree and provide the value
            const auto value = idtx::aas_json::getString(root, idtx::aas_json::Path::parse(jsonPath));
            ctx.SetOutput<std::string>(value.value_or(""));
        })
        .Inputs(
            Attribute(IDTXTokens->inputsJsonData)
                .Connections<std::string>(ExecBuiltinComputations->computeValue)
                .InputName(IDTXTokens->inputsJsonData),
            AttributeValue<std::string>(IDTXTokens->jsonPath)
                .Required()
        );

    // Register the computed value as computation to make it accessible via ConnectionTargetedObjects
    self.AttributeComputation(IDTXTokens->outputsJsonValueString, _IDTXAASTokens->resolvedValue)
        .Callback<std::string>(+[](const VdfContext &ctx)
        {
            ctx.SetOutput<std::string>(
                ctx.GetInputValue<std::string>(IDTXTokens->outputsJsonValueString)
            );
        })
        .Inputs(
            Prim().Computation<std::string>(IDTXTokens->outputsJsonValueString)
        );
}
